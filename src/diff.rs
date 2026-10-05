//! The differential runner: each statement of a file on both servers, compared.
//!
//! A file runs in three passes, one per way that a driver sends a statement: the simple query,
//! the extended query with text results and the extended query with binary results, per document
//! 14 section 14.5 of the notes. Each pass runs the whole file in order in one session, on a new
//! database on each server, so the passes do not see each other's state. The fourth way, with
//! parameters, comes with the generator.
//!
//! When a statement differs, the state of the two servers can differ after it. So the first
//! difference in a pass is a finding, and each later difference in the same pass is counted as
//! downstream of it. A downstream difference is still a statement that failed.

use std::io;
use std::path::Path;

use crate::client::{Client, Login, Transport};
use crate::compare::{self, Difference};
use crate::frame::{self, Frame};
use crate::json::Json;
use crate::servers::Server;
use crate::sql::{self, Item};

/// The database that a pass runs in. It is dropped and made again from `template0` before
/// each pass.
const DATABASE: &str = "rudb_postgres_diff";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Simple,
    Text,
    Binary,
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::Simple, Mode::Text, Mode::Binary];

    fn name(self) -> &'static str {
        match self {
            Mode::Simple => "simple",
            Mode::Text => "text",
            Mode::Binary => "binary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Same,
    Different,
    Downstream,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Outcome::Same => "same",
            Outcome::Different => "different",
            Outcome::Downstream => "downstream",
        }
    }
}

#[derive(Debug)]
struct Result {
    mode: Mode,
    line: usize,
    statement: String,
    ordered: bool,
    outcome: Outcome,
    differences: Vec<Difference>,
}

#[derive(Debug, Clone)]
pub(crate) struct Options {
    /// Run as `rpg` over TCP, and not as the superuser over the socket.
    pub(crate) rpg: bool,
}

/// Runs a file and returns the number of statements that differ in any pass, and the result as
/// JSON for the results file.
pub(crate) fn run(
    file: &Path,
    oracle: &Server,
    other: &Server,
    options: &Options,
) -> std::result::Result<(usize, Json), String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let items = sql::split(&text);
    let meta = items.iter().filter(|i| matches!(i, Item::Meta { .. })).count();
    let statements: Vec<(usize, String, Option<String>)> = items
        .into_iter()
        .filter_map(|item| match item {
            Item::Statement { line, text, copy_data } => Some((line, text, copy_data)),
            Item::Meta { .. } => None,
        })
        .collect();
    println!("{}: {} statements, {meta} meta-commands not run", file.display(), statements.len());

    let mut results = Vec::new();
    for mode in Mode::ALL {
        let mut a = session(oracle, options)?;
        let mut b = session(other, options)?;
        let mut first = true;
        for (line, statement, copy_data) in &statements {
            let ordered = sql::has_top_level_order_by(statement);
            let x = exchange(&mut a, mode, statement, copy_data.as_deref())
                .map_err(|e| format!("{}: {e}", oracle.name))?;
            let y = exchange(&mut b, mode, statement, copy_data.as_deref())
                .map_err(|e| format!("{}: {e}", other.name))?;
            let differences = compare::answer(&x, &y, ordered);
            let outcome = match (differences.is_empty(), first) {
                (true, _) => Outcome::Same,
                (false, true) => Outcome::Different,
                (false, false) => Outcome::Downstream,
            };
            if outcome != Outcome::Same {
                first = false;
            }
            results.push(Result {
                mode,
                line: *line,
                statement: statement.clone(),
                ordered,
                outcome,
                differences,
            });
        }
    }

    println!("{:<8} {:>6} {:>10} {:>11}", "mode", "same", "different", "downstream");
    for mode in Mode::ALL {
        let count =
            |o: Outcome| results.iter().filter(|r| r.mode == mode && r.outcome == o).count();
        println!(
            "{:<8} {:>6} {:>10} {:>11}",
            mode.name(),
            count(Outcome::Same),
            count(Outcome::Different),
            count(Outcome::Downstream)
        );
    }
    for r in results.iter().filter(|r| r.outcome == Outcome::Different) {
        println!("\n{}, line {}: {}", r.mode.name(), r.line, short(&r.statement));
        for d in &r.differences {
            println!(
                "  frame {}, {}:\n    {}: {}\n    {}: {}",
                d.at, d.what, oracle.name, d.oracle, other.name, d.other
            );
        }
    }

    let mut differing: Vec<usize> =
        results.iter().filter(|r| r.outcome != Outcome::Same).map(|r| r.line).collect();
    differing.sort_unstable();
    differing.dedup();
    let json = Json::object(vec![
        ("file", Json::String(file.display().to_string())),
        ("oracle", Json::str(&oracle.name)),
        ("other", Json::str(&other.name)),
        ("statements", Json::Number(statements.len() as i64)),
        ("meta_commands_not_run", Json::Number(meta as i64)),
        ("results", Json::Array(results.iter().map(result_json).collect())),
    ]);
    Ok((differing.len(), json))
}

fn result_json(r: &Result) -> Json {
    Json::object(vec![
        ("mode", Json::str(r.mode.name())),
        ("line", Json::Number(r.line as i64)),
        ("statement", Json::str(&r.statement)),
        ("ordered", Json::Bool(r.ordered)),
        ("outcome", Json::str(r.outcome.name())),
        (
            "differences",
            Json::Array(
                r.differences
                    .iter()
                    .map(|d| {
                        Json::object(vec![
                            ("frame", Json::Number(d.at as i64)),
                            ("what", Json::str(&d.what)),
                            ("oracle", Json::str(&d.oracle)),
                            ("other", Json::str(&d.other)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

/// Makes the pass database again and opens a session in it.
fn session(server: &Server, options: &Options) -> std::result::Result<Client, String> {
    let mut admin =
        Client::connect(server, &Login::superuser("postgres")).map_err(|r| r.message)?;
    for sql in [
        format!("DROP DATABASE IF EXISTS {DATABASE} WITH (FORCE)"),
        format!("CREATE DATABASE {DATABASE} TEMPLATE template0 OWNER rpg"),
    ] {
        let reply = admin.simple(&sql).map_err(|e| format!("{}: {e}", server.name))?;
        if let Some(error) = reply.iter().find(|f| f.tag == b'E') {
            return Err(format!("{}: {sql}: {}", server.name, crate::client::error_text(error)));
        }
    }
    let login = if options.rpg {
        Login { transport: Transport::Tcp, ..Login::rpg(DATABASE) }
    } else {
        Login::superuser(DATABASE)
    };
    Client::connect(server, &login).map_err(|r| r.message)
}

/// Sends one statement in one mode and reads the reply up to `ReadyForQuery`. A `COPY FROM
/// STDIN` gets the data that followed it in the file, or a `CopyFail` when there was none.
fn exchange(
    client: &mut Client,
    mode: Mode,
    statement: &str,
    copy_data: Option<&str>,
) -> io::Result<Vec<Frame>> {
    match mode {
        Mode::Simple => client.send(&[frame::query(statement)])?,
        Mode::Text | Mode::Binary => {
            let format = u16::from(mode == Mode::Binary);
            client.send(&[
                frame::parse("", statement, &[]),
                frame::bind("", "", 0, &[], format),
                frame::describe(b'P', ""),
                frame::execute("", 0),
                frame::sync(),
            ])?;
        }
    }
    let mut frames = Vec::new();
    loop {
        let f = client.read()?;
        let tag = f.tag;
        frames.push(f);
        match tag {
            b'G' => {
                match copy_data {
                    Some(data) => {
                        client.send(&[frame::copy_data(data.as_bytes()), frame::copy_done()])?
                    }
                    None => {
                        client.send(&[frame::copy_fail("the file has no data for this COPY")])?
                    }
                }
                // The server ignores a Sync that arrives in the copy-in state, so the extended
                // query needs one more after the copy.
                if mode != Mode::Simple {
                    client.send(&[frame::sync()])?;
                }
            }
            b'Z' => return Ok(frames),
            _ => {}
        }
    }
}

fn short(statement: &str) -> String {
    let one_line = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 120 {
        format!("{}...", one_line.chars().take(120).collect::<String>())
    } else {
        one_line
    }
}
