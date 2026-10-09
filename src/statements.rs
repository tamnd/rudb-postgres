//! The statement number of the core regression suite, per document 14 section 14.8 of the notes.
//!
//! The runner starts the recording proxy on a Unix socket in front of the oracle and runs
//! `pg_regress` through it, so that each psql session of the suite has a trace. `pg_regress` sets
//! `application_name` to `pg_regress/<test>` for each test, so the startup message of a trace
//! names its test. Each `Query` in a trace is one statement as psql sent it, after psql expanded
//! its variables and ran its meta-commands. A run of extended query messages up to `Sync`, and a
//! `FunctionCall`, are one statement each too. The `CopyData`, `CopyDone` and `CopyFail` messages
//! after a statement go with it.
//!
//! The runner then replays the sessions, one at a time in the order in which they started, in
//! the database and as the user of the recording. Each session runs on both servers at the same
//! time, one statement after the other. It compares the answer of each
//! statement under document 01 section 1.5, with the map of user OIDs of [`crate::replay`]. In
//! each test, the first statement that differs is a finding, and each later difference in the same
//! test is counted as downstream of it, as in [`crate::diff`].
//!
//! A statement under an item of document 01 section 1.6 is counted in a column of its own and not
//! in the denominator: `EXPLAIN` (item 1), and `LOAD` and `LANGUAGE C` (item 6).

use std::path::Path;

use crate::client::{Client, Login, Transport};
use crate::compare::{self, Difference};
use crate::frame::{self, Frame};
use crate::json::Json;
use crate::proxy::{self, Address};
use crate::regress::{self, Suite};
use crate::replay::{self, Oids};
use crate::servers::Server;
use crate::sql;
use crate::trace::{self, Line, Style};

/// The name of the traces, and of the server that `pg_regress` sees in front of the oracle.
const NAME: &str = "regress";

/// The `application_name` of the sessions that `pg_regress` itself opens, for example to make the
/// database `regression`.
const DRIVER: &str = "pg_regress";

#[derive(Debug, Default)]
pub(crate) struct Options {
    /// Run these tests and not the schedule.
    pub(crate) tests: Vec<String>,
}

/// One psql session of the recording.
struct Session {
    /// The number of the trace, which is the order in which the sessions started.
    number: usize,
    test: String,
    user: String,
    database: String,
    /// The other parameters of the startup message.
    options: Vec<(String, String)>,
    statements: Vec<Statement>,
}

struct Statement {
    /// The messages that the client sent, up to and including `Query`, `FunctionCall` or `Sync`.
    send: Vec<Frame>,
    /// The `CopyData`, `CopyDone` and `CopyFail` messages after it.
    copy: Vec<Frame>,
    /// The backend lines of the recording, for the map of OIDs.
    lines: Vec<Vec<u8>>,
    /// The SQL text, for the report and to find the items of section 1.6.
    text: String,
    /// False for a run of extended query messages that the trace ends before its `Sync`.
    complete: bool,
}

/// What one server sent for a statement, or why it sent nothing.
type Answer = Result<Vec<Frame>, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Same,
    Different,
    Downstream,
    /// Under an item of document 01 section 1.6.
    Excluded,
}

#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    same: usize,
    different: usize,
    downstream: usize,
    excluded: usize,
}

impl Counts {
    fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Same => self.same += 1,
            Outcome::Different => self.different += 1,
            Outcome::Downstream => self.downstream += 1,
            Outcome::Excluded => self.excluded += 1,
        }
    }

    fn counted(&self) -> usize {
        self.same + self.different + self.downstream
    }

    fn json(&self) -> Vec<(&'static str, Json)> {
        vec![
            ("same", Json::Number(self.same as i64)),
            ("different", Json::Number(self.different as i64)),
            ("downstream", Json::Number(self.downstream as i64)),
            ("excluded", Json::Number(self.excluded as i64)),
        ]
    }
}

/// The first difference of a test.
struct Finding {
    session: usize,
    statement: String,
    differences: Vec<Difference>,
}

/// Records the suite on the oracle, replays it on both servers, and returns the number of
/// statements that differ and the result JSON.
pub(crate) fn run(
    root: &Path,
    prefix: &Path,
    oracle: &Server,
    other: &Server,
    options: &Options,
) -> Result<(usize, Json), String> {
    let sessions = record(root, prefix, oracle, &options.tests)?;
    let count: usize = sessions.iter().map(|s| s.statements.len()).sum();
    println!("recorded {} sessions with {count} statements", sessions.len());

    // Both servers replay each session at the same time, one statement after the other, so that
    // the runner keeps only the answers of one statement.
    regress::clean(oracle);
    regress::clean(other);
    let mut tests: Vec<(String, Counts, Option<Finding>)> = Vec::new();
    for session in &sessions {
        // The tests of a parallel group interleave their sessions, so a later session can be of
        // a test that is not the last one.
        let i = match tests.iter().position(|(test, _, _)| *test == session.test) {
            Some(i) => i,
            None => {
                println!("replaying {}", session.test);
                tests.push((session.test.clone(), Counts::default(), None));
                tests.len() - 1
            }
        };
        let (_, counts, finding) = &mut tests[i];
        let (mut a, mut b) = (Player::open(oracle, session), Player::open(other, session));
        for statement in &session.statements {
            let (x, y) = (a.answer(statement), b.answer(statement));
            let differences = if excluded(&statement.text) {
                None
            } else {
                Some(differ(&x, &y, sql::has_top_level_order_by(&statement.text)))
            };
            let outcome = match differences {
                None => Outcome::Excluded,
                Some(d) if d.is_empty() => Outcome::Same,
                Some(_) if finding.is_some() => Outcome::Downstream,
                Some(differences) => {
                    *finding = Some(Finding {
                        session: session.number,
                        statement: statement.text.clone(),
                        differences,
                    });
                    Outcome::Different
                }
            };
            counts.add(outcome);
        }
        a.close();
        b.close();
    }

    let mut total = Counts::default();
    println!(
        "{:<28} {:>6} {:>10} {:>11} {:>9}",
        "test", "same", "different", "downstream", "excluded"
    );
    for (test, counts, _) in &tests {
        println!(
            "{:<28} {:>6} {:>10} {:>11} {:>9}",
            test, counts.same, counts.different, counts.downstream, counts.excluded
        );
        total.same += counts.same;
        total.different += counts.different;
        total.downstream += counts.downstream;
        total.excluded += counts.excluded;
    }
    println!(
        "statements: {} of {} the same on {} ({:.2} percent), {} excluded under section 1.6",
        total.same,
        total.counted(),
        other.name,
        percent(total.same, total.counted()),
        total.excluded
    );

    let mut fields = vec![
        ("oracle", Json::str(&oracle.name)),
        ("other", Json::str(&other.name)),
        ("statements", Json::Number(total.counted() as i64)),
    ];
    fields.extend(total.json());
    fields.push((
        "tests",
        Json::Array(
            tests
                .iter()
                .map(|(test, counts, finding)| {
                    let mut fields = vec![("test", Json::str(test))];
                    fields.extend(counts.json());
                    if let Some(f) = finding {
                        fields.push(("first", finding_json(f)));
                    }
                    Json::object(fields)
                })
                .collect(),
        ),
    ));
    Ok((total.different + total.downstream, Json::object(fields)))
}

fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 * 100.0 / whole as f64 }
}

fn finding_json(f: &Finding) -> Json {
    Json::object(vec![
        ("session", Json::Number(f.session as i64)),
        ("statement", Json::str(&f.statement)),
        (
            "differences",
            Json::Array(
                f.differences
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

/// The differences between two answers. A server that sent nothing differs from one that sent
/// frames, and two servers that both sent nothing do not differ.
fn differ(x: &Answer, y: &Answer, ordered: bool) -> Vec<Difference> {
    match (x, y) {
        (Ok(x), Ok(y)) => compare::answer(x, y, ordered),
        (Err(_), Err(_)) => Vec::new(),
        (x, y) => {
            let show = |a: &Answer| match a {
                Ok(frames) => format!("{} frames", frames.len()),
                Err(why) => why.clone(),
            };
            vec![Difference { at: 0, what: "answer".to_string(), oracle: show(x), other: show(y) }]
        }
    }
}

/// Whether a statement is under an item of document 01 section 1.6: `EXPLAIN` (item 1), or `LOAD`
/// and a function in C (item 6).
fn excluded(text: &str) -> bool {
    let words: Vec<String> = strip_comments(text)
        .split(|c: char| c.is_whitespace() || c == ';' || c == '(')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    match words.first().map(String::as_str) {
        Some("explain" | "load") => true,
        Some("create") => words
            .windows(2)
            .any(|w| w[0] == "language" && matches!(w[1].as_str(), "c" | "'c'" | "\"c\"")),
        _ => false,
    }
}

/// The text without its SQL comments, so that the first word is the first word of the statement.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let line = rest.find("--");
        let block = rest.find("/*");
        match (line, block) {
            (Some(l), b) if b.is_none_or(|b| l < b) => {
                out.push_str(&rest[..l]);
                rest = rest[l..].find('\n').map_or("", |n| &rest[l + n..]);
            }
            (_, Some(b)) => {
                out.push_str(&rest[..b]);
                out.push(' ');
                rest = rest[b..].find("*/").map_or("", |e| &rest[b + e + 2..]);
            }
            _ => {
                out.push_str(rest);
                return out;
            }
        }
    }
}

/// Runs `pg_regress` through the proxy in front of the oracle and reads the traces.
fn record(
    root: &Path,
    prefix: &Path,
    oracle: &Server,
    tests: &[String],
) -> Result<Vec<Session>, String> {
    let dir = root.join("run").join("statements");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
    }
    let (traces, socket) = (dir.join("traces"), dir.join("sock"));
    std::fs::create_dir_all(&socket)
        .map_err(|e| format!("cannot create {}: {e}", socket.display()))?;

    let front = Server {
        name: NAME.to_string(),
        socket: socket.clone(),
        port: proxy::DEFAULT_PORT,
        ..oracle.clone()
    };
    let options =
        proxy::Options { name: NAME.to_string(), style: Style::Exact, ..Default::default() };
    let recorder = proxy::start(
        &traces,
        Address::socket(&socket, front.port),
        Address::socket(&oracle.socket, oracle.port),
        &options,
    )?;
    let suite: &Suite = &regress::REGRESS;
    let run = regress::drive(root, prefix, suite, &front, tests);
    recorder.stop()?;
    let run = run?;
    println!(
        "pg_regress through the proxy: {}/{} tests passed on {}",
        run.passed(),
        run.outcomes.len(),
        oracle.name
    );

    let mut sessions = Vec::new();
    for entry in
        std::fs::read_dir(&traces).map_err(|e| format!("cannot read {}: {e}", traces.display()))?
    {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(number) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix(&format!("{NAME}-")))
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        let text =
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let lines = trace::read(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(session) = session(number, lines) {
            sessions.push(session);
        }
    }
    sessions.sort_by_key(|s| s.number);
    Ok(sessions)
}

/// The session of a trace, when it is a session of `pg_regress` that logged in. The sessions of
/// the runner itself have no `application_name` of `pg_regress`.
fn session(number: usize, lines: Vec<Line>) -> Option<Session> {
    let mut startup = None;
    let mut ready = false;
    let mut statements: Vec<Statement> = Vec::new();
    // Whether the last statement is a run of extended query messages without its `Sync` yet.
    let mut open = false;
    for line in lines {
        match line {
            Line::Frontend(bytes) if !ready => {
                if startup.is_none() && replay::is_startup(&bytes) {
                    startup = Some(bytes);
                }
            }
            Line::Backend(line) if !ready => ready = line.starts_with(b"B\t5\tReadyForQuery\t"),
            Line::Frontend(bytes) if replay::is_cancel(&bytes) => {}
            Line::Frontend(bytes) => {
                let Ok(f) = frame::read(&mut bytes.as_slice()) else { continue };
                match f.tag {
                    b'X' => break,
                    b'd' | b'c' | b'f' => {
                        if let Some(last) = statements.last_mut() {
                            last.copy.push(f);
                        }
                    }
                    b'Q' | b'F' => {
                        let text = query_text(&f);
                        statements.push(Statement {
                            send: vec![f],
                            copy: Vec::new(),
                            lines: Vec::new(),
                            text,
                            complete: true,
                        });
                        open = false;
                    }
                    tag => {
                        let last = match statements.last_mut() {
                            Some(last) if open => last,
                            _ => {
                                statements.push(Statement {
                                    send: Vec::new(),
                                    copy: Vec::new(),
                                    lines: Vec::new(),
                                    text: String::new(),
                                    complete: false,
                                });
                                statements.last_mut()?
                            }
                        };
                        if tag == b'P' && last.text.is_empty() {
                            last.text = query_text(&f);
                        }
                        last.send.push(f);
                        open = tag != b'S';
                        last.complete = !open;
                    }
                }
            }
            Line::Backend(line) => {
                if let Some(last) = statements.last_mut() {
                    last.lines.push(line);
                }
            }
        }
    }
    if !ready {
        return None;
    }
    let startup = startup?;
    let mut fields =
        startup[8..].split(|b| *b == 0).map(|f| String::from_utf8_lossy(f).into_owned());
    let (mut user, mut database, mut test) = (String::new(), String::new(), None);
    let mut options = Vec::new();
    while let (Some(name), Some(value)) = (fields.next(), fields.next()) {
        match name.as_str() {
            "" => break,
            "user" => user = value,
            "database" => database = value,
            _ => {
                if name == "application_name" {
                    test = test_of(&value);
                }
                options.push((name, value));
            }
        }
    }
    if database.is_empty() {
        database.clone_from(&user);
    }
    statements.retain(|s| s.complete);
    Some(Session { number, test: test?, user, database, options, statements })
}

/// The test of an `application_name`: `pg_regress/<test>` for a test, and `pg_regress` for the
/// sessions of the driver.
fn test_of(application_name: &str) -> Option<String> {
    match application_name.strip_prefix(DRIVER) {
        Some("") => Some(DRIVER.to_string()),
        Some(rest) => rest.strip_prefix('/').map(str::to_string),
        None => None,
    }
}

/// The SQL text of a `Query` or a `Parse`, and the empty text for any other message.
fn query_text(f: &Frame) -> String {
    let mut fields = f.fields();
    let sql = match f.tag {
        b'Q' => fields.cstr(),
        b'P' => fields.cstr().and_then(|_| fields.cstr()),
        _ => None,
    };
    sql.map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default()
}

/// One session of the replay on one server. After the server refuses the login, closes the
/// connection or stops answering, each later statement of the session has no answer.
struct Player {
    client: Result<Client, String>,
    oids: Oids,
}

impl Player {
    fn open(server: &Server, session: &Session) -> Player {
        let login = Login {
            user: session.user.clone(),
            password: String::new(),
            database: session.database.clone(),
            transport: Transport::Unix,
            options: session.options.clone(),
        };
        let client = Client::connect(server, &login)
            .map_err(|refused| format!("the login failed: {}", refused.message));
        Player { client, oids: Oids::default() }
    }

    fn answer(&mut self, statement: &Statement) -> Answer {
        let client = self.client.as_mut().map_err(|why| why.clone())?;
        match exchange(client, &self.oids, statement) {
            Ok(mut frames) => {
                self.oids.backend(&mut frames, &statement.lines);
                Ok(frames)
            }
            Err(e) => {
                let why = match e.kind() {
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                        "the server did not answer".to_string()
                    }
                    _ => "the server closed the connection".to_string(),
                };
                self.client = Err(why.clone());
                Err(why)
            }
        }
    }

    fn close(self) {
        if let Ok(mut client) = self.client {
            let _ = client.send(&[frame::terminate()]);
        }
    }
}

/// Sends a statement and reads the answer up to `ReadyForQuery`. When the server asks for the data
/// of a `COPY FROM STDIN`, it gets the copy messages of the recording, or a `CopyFail` when the
/// recording has none, because the oracle did not ask.
fn exchange(
    client: &mut Client,
    oids: &Oids,
    statement: &Statement,
) -> std::io::Result<Vec<Frame>> {
    let send: Vec<Frame> = statement.send.iter().map(|f| oids.frontend(f)).collect();
    client.send(&send)?;
    let mut frames = Vec::new();
    loop {
        let f = client.read()?;
        let tag = f.tag;
        frames.push(f);
        match tag {
            b'G' if statement.copy.is_empty() => {
                client.send(&[frame::copy_fail("the recording has no data for this COPY")])?
            }
            b'G' => client.send(&statement.copy)?,
            b'Z' => return Ok(frames),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontend(f: &Frame) -> Line {
        Line::Frontend(f.bytes())
    }

    fn backend(text: &str) -> Line {
        Line::Backend(text.as_bytes().to_vec())
    }

    fn startup(fields: &[(&str, &str)]) -> Line {
        let mut body = vec![0, 3, 0, 0];
        for (name, value) in fields {
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            body.extend_from_slice(value.as_bytes());
            body.push(0);
        }
        body.push(0);
        let mut bytes = ((body.len() + 4) as u32).to_be_bytes().to_vec();
        bytes.extend(body);
        Line::Frontend(bytes)
    }

    #[test]
    fn a_trace_gives_its_test_and_its_statements() {
        let lines = vec![
            startup(&[
                ("user", "postgres"),
                ("database", "regression"),
                ("application_name", "pg_regress/int4"),
            ]),
            backend("B\t5\tReadyForQuery\t I"),
            frontend(&frame::query("copy t from stdin")),
            backend("B\t7\tCopyInResponse\t 0 1 0"),
            frontend(&frame::copy_data(b"1\n")),
            frontend(&frame::copy_done()),
            backend("B\t5\tReadyForQuery\t I"),
            frontend(&frame::parse("", "select $1", &[])),
            frontend(&frame::bind("", "", 0, &[], 0)),
            frontend(&frame::execute("", 0)),
            frontend(&frame::sync()),
            backend("B\t5\tReadyForQuery\t I"),
            frontend(&frame::parse("", "select 2", &[])),
            frontend(&frame::terminate()),
        ];
        let session = session(4, lines).unwrap();
        assert_eq!(session.test, "int4");
        assert_eq!((session.user.as_str(), session.database.as_str()), ("postgres", "regression"));
        let texts: Vec<&str> = session.statements.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["copy t from stdin", "select $1"], "a run without Sync is not played");
        assert_eq!(session.statements[0].copy.len(), 2);
        assert_eq!(session.statements[1].send.len(), 4);
    }

    #[test]
    fn a_session_of_the_runner_is_not_a_test() {
        let lines = vec![
            startup(&[("user", "postgres"), ("database", "postgres")]),
            backend("B\t5\tReadyForQuery\t I"),
        ];
        assert!(session(1, lines).is_none());
        assert_eq!(test_of("pg_regress"), Some("pg_regress".to_string()));
        assert_eq!(test_of("pg_regress/boolean"), Some("boolean".to_string()));
        assert_eq!(test_of("pg_regressx"), None);
        assert_eq!(test_of("psql"), None);
    }

    #[test]
    fn the_items_of_section_1_6_are_excluded() {
        assert!(excluded("EXPLAIN (costs off) select 1"));
        assert!(excluded("-- a plan\n/* here */ explain select 1"));
        assert!(excluded("LOAD 'plpgsql'"));
        assert!(excluded("CREATE FUNCTION f() RETURNS int AS 'regress.so' LANGUAGE C STRICT"));
        assert!(excluded("create function f() returns int as 'x' language 'c'"));
        assert!(!excluded("CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'select 1'"));
        assert!(!excluded("select 'explain'"));
        assert!(!excluded("select 1 -- explain"));
    }
}
