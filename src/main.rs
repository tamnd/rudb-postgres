//! The command line of the PostgreSQL compatibility harness.
//!
//! Each subcommand prints its result and exits with a non-zero code when it finds a difference
//! that is not in an expected list. The design is document 14 of the PostgreSQL compatibility
//! notes, and the layout of this crate follows its section 14.2.

use std::path::PathBuf;
use std::process::ExitCode;

mod client;
mod compare;
mod connect;
mod crypto;
mod diff;
mod drivers;
mod frame;
mod generate;
mod json;
mod oracle;
mod perf;
mod pins;
mod process;
mod proxy;
mod regress;
mod replay;
mod report;
mod scram;
mod servers;
mod sql;
mod toml;
mod trace;
mod upstream;

const USAGE: &str = "\
usage: rudb-postgres <command> [options]

commands:
  oracle build          build PostgreSQL from the pin into target/oracle/<pin>
  up [--rudb <binary>] [--port <n>] [--fresh]
                        start the oracle on port n and the other server on n + 1; the
                        other server is rudb, or a second oracle without --rudb
  down                  stop the servers that up started
  connect               log in to both servers each way and compare the replies
  record --to <server> [--port <n>] [--name <name>] [--sessions <n>] [--regress]
                        start the proxy on port n in front of a server and write a trace
                        per session to run/traces/<name>-<i>.trace; --sessions stops after
                        n sessions, and --regress writes the regress style of libpq
  replay <trace>...     replay each trace against both servers and compare the replies
  diff [--rpg] <file.sql>...
                        run each statement on both servers in three modes and compare
                        the answers; --rpg runs as rpg over tcp, not as the superuser
  gen [--seed <n>] [--count <n>]
                        generate values, function calls and random queries from a seed,
                        run them through diff, and run the logic checks on each server
  client [--accept] [<name>...]
                        run the test suite of each client in clients/, or of the named
                        clients, against both servers; --accept writes the failure lists
  regress [--accept] [<test>...]
                        run the core regression suite with pg_regress on both servers;
                        --accept writes the diffs of rudb to corpus/regress
  isolation [--accept] [<spec>...]
                        run the isolation specs with pg_isolation_regress on both servers
  drivers [<name>...]   run the smoke script of each client in drivers/, or of the named
                        clients, against both servers; each one connects over TLS with
                        SCRAM and runs SELECT 1
  perf [--seconds <n>] [--idle <n>]
                        measure the SELECT 1 floor, the connection rate with trust and
                        with SCRAM, and the memory of an idle session on both servers;
                        each timed row runs n seconds on each server, and the memory row
                        opens up to n idle sessions
  report [--rudb [<commit>]] [--date <yyyy-mm-dd>]
                        count the passed cases of each denominator from run/results and
                        write the page; with --rudb, the page of that rudb commit, or of the
                        commit in pins.toml, goes to reports/<date>, and without it, the page
                        of the twin goes to run/report
";

/// The exit code for a command that is not written yet or that was called wrongly. It is not 1,
/// so that a script can tell a missing command from a difference.
const NOT_WRITTEN: u8 = 2;

/// The exit code for a harness that could not do its job, for example when the oracle is not
/// built. A run that ends here has no result at all.
const BROKEN: u8 = 3;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::from(NOT_WRITTEN);
    };
    match command.as_str() {
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        "oracle" if args.get(1).map(String::as_str) == Some("build") => {
            report(pins::root().and_then(|root| oracle::build(&root, &pins::Pins::read(&root)?)))
        }
        "up" => match up_options(&args[1..]) {
            Ok(options) => report(
                pins::root()
                    .and_then(|root| servers::up(&root, &pins::Pins::read(&root)?, &options)),
            ),
            Err(error) => {
                eprintln!("rudb-postgres up: {error}");
                ExitCode::from(NOT_WRITTEN)
            }
        },
        "down" => report(pins::root().and_then(|root| servers::down(&root))),
        "connect" => match connect_command() {
            Ok(count) => differences(count),
            Err(error) => report(Err(error)),
        },
        "diff" => match diff_command(&args[1..]) {
            Ok(count) => differences(count),
            Err(error) => report(Err(error)),
        },
        "regress" | "isolation" => {
            let suite = if command == "regress" { &regress::REGRESS } else { &regress::ISOLATION };
            match regress_command(suite, &args[1..]) {
                Ok(count) => differences(count),
                Err(error) => report(Err(error)),
            }
        }
        "record" => match record_command(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => report(Err(error)),
        },
        "replay" => match replay_command(&args[1..]) {
            Ok(count) => differences(count),
            Err(error) => report(Err(error)),
        },
        "gen" => match gen_command(&args[1..]) {
            Ok(count) => differences(count),
            Err(error) => report(Err(error)),
        },
        "report" => report(report_command(&args[1..])),
        "perf" => report(perf_command(&args[1..])),
        "drivers" => match drivers_command(&args[1..]) {
            Ok(count) => differences(count),
            Err(error) => report(Err(error)),
        },
        "client" => match client_command(&args[1..]) {
            Ok(count) => differences(count),
            Err(error) => report(Err(error)),
        },
        "oracle" => {
            eprintln!("rudb-postgres {command}: not written yet, see tamnd/rudb#2488");
            ExitCode::from(NOT_WRITTEN)
        }
        other => {
            eprintln!("rudb-postgres: unknown command {other:?}\n");
            eprint!("{USAGE}");
            ExitCode::from(NOT_WRITTEN)
        }
    }
}

fn report(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rudb-postgres: {error}");
            ExitCode::from(BROKEN)
        }
    }
}

/// Writes the report page.
fn report_command(args: &[String]) -> Result<(), String> {
    let root = pins::root()?;
    let pins = pins::Pins::read(&root)?;
    let mut options = report::Options::default();
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rudb" => {
                let commit = match args.next_if(|a| !a.starts_with("--")) {
                    Some(commit) => commit.clone(),
                    None => pins.rudb.clone(),
                };
                if commit.len() < 8 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(format!("{commit:?} is not a commit hash"));
                }
                options.rudb = Some(commit);
            }
            "--date" => options.date = Some(args.next().ok_or("--date needs a value")?.clone()),
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    let (json, markdown) = report::run(&root, &pins, &options)?;
    println!("{}\n{}", json.display(), markdown.display());
    Ok(())
}

/// Runs the suite of each client and writes `run/results/client-<name>.json`.
fn client_command(args: &[String]) -> Result<usize, String> {
    let mut options = upstream::Options::default();
    let mut names = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--accept" => options.accept = true,
            other if other.starts_with("--") => return Err(format!("unknown option {other:?}")),
            name => names.push(name.to_string()),
        }
    }
    let root = pins::root()?;
    if names.is_empty() {
        names = upstream::names(&root)?;
    }
    let servers = servers::load(&root)?;
    let mut count = 0;
    for name in &names {
        let (n, json) = upstream::run(&root, name, &servers[0], &servers[1], &options)?;
        write_result(&root, &format!("client-{name}"), &json)?;
        count += n;
    }
    Ok(count)
}

/// Runs the performance rows and writes `run/results/perf.json`.
fn perf_command(args: &[String]) -> Result<(), String> {
    let mut options = perf::Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let value = args.next().ok_or(format!("{arg} needs a number"))?;
        match arg.as_str() {
            "--seconds" => {
                options.seconds =
                    value.parse().map_err(|_| format!("{value:?} is not a number"))?;
            }
            "--idle" => {
                options.idle = value.parse().map_err(|_| format!("{value:?} is not a number"))?;
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    let root = pins::root()?;
    let servers = servers::load(&root)?;
    let json = perf::run(&servers[0], &servers[1], &options)?;
    write_result(&root, "perf", &json)
}

/// Runs the driver gate and writes `run/results/drivers.json`.
fn drivers_command(args: &[String]) -> Result<usize, String> {
    if let Some(option) = args.iter().find(|a| a.starts_with("--")) {
        return Err(format!("unknown option {option:?}"));
    }
    let root = pins::root()?;
    let names = if args.is_empty() { drivers::names(&root)? } else { args.to_vec() };
    let servers = servers::load(&root)?;
    let (count, json) = drivers::run(&root, &names, &servers[0], &servers[1]);
    write_result(&root, "drivers", &json)?;
    Ok(count)
}

/// Runs the connect matrix and writes `run/results/connect.json`.
fn connect_command() -> Result<usize, String> {
    let root = pins::root()?;
    let servers = servers::load(&root)?;
    let (count, json) = connect::run(&servers[0], &servers[1]);
    write_result(&root, "connect", &json)?;
    Ok(count)
}

/// Writes `run/results/<name>.json`, which the report reads.
fn write_result(root: &std::path::Path, name: &str, json: &json::Json) -> Result<(), String> {
    let dir = root.join("run").join("results");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, json.pretty())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Runs `diff` on each file and writes `run/results/diff.json`.
fn diff_command(args: &[String]) -> Result<usize, String> {
    let rpg = args.iter().any(|a| a == "--rpg");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if files.is_empty() {
        return Err("diff needs at least one file".to_string());
    }
    let root = pins::root()?;
    let servers = servers::load(&root)?;
    let mut total = 0;
    let mut results = Vec::new();
    for file in files {
        let (count, json) = diff::run(
            std::path::Path::new(file),
            &servers[0],
            &servers[1],
            &diff::Options { rpg },
        )?;
        total += count;
        results.push(json);
    }
    write_result(&root, "diff", &json::Json::Array(results))?;
    Ok(total)
}

/// Runs the generator and writes `run/results/gen.json`. Without `--seed`, the seed comes from
/// the clock, and the output gives it, so that a run can be made again.
fn gen_command(args: &[String]) -> Result<usize, String> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_secs());
    let mut options = generate::Options { seed, count: 200 };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let value = args.next().ok_or(format!("{arg} needs a number"))?;
        let number = value.parse().map_err(|_| format!("{value:?} is not a number"))?;
        match arg.as_str() {
            "--seed" => options.seed = number,
            "--count" => options.count = number as usize,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    let root = pins::root()?;
    let servers = servers::load(&root)?;
    let (count, json) = generate::run(&root, &servers[0], &servers[1], &options)?;
    write_result(&root, "gen", &json)?;
    Ok(count)
}

/// Runs the recording proxy until it has recorded the sessions, or until it is stopped.
fn record_command(args: &[String]) -> Result<(), String> {
    let mut options = proxy::Options::default();
    let mut to = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().cloned().ok_or(format!("{arg} needs {what}"));
        match arg.as_str() {
            "--to" => to = Some(value("a server name")?),
            "--name" => options.name = value("a name")?,
            "--port" => {
                let port = value("a number")?;
                options.port = port.parse().map_err(|_| format!("{port:?} is not a port"))?;
            }
            "--sessions" => {
                let n = value("a number")?;
                options.sessions = Some(n.parse().map_err(|_| format!("{n:?} is not a number"))?);
            }
            "--regress" => options.style = trace::Style::Regress,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    let to = to.ok_or("record needs --to <server>")?;
    let root = pins::root()?;
    let servers = servers::load(&root)?;
    let server = servers.iter().find(|s| s.name == to).ok_or(format!("no server {to:?} is up"))?;
    proxy::record(&root, server, &options)
}

/// Replays each trace and writes `run/results/replay.json`.
fn replay_command(files: &[String]) -> Result<usize, String> {
    if files.is_empty() {
        return Err("replay needs at least one trace".to_string());
    }
    let root = pins::root()?;
    let servers = servers::load(&root)?;
    let mut total = 0;
    let mut results = Vec::new();
    for file in files {
        let (count, json) = replay::run(std::path::Path::new(file), &servers[0], &servers[1])?;
        total += count;
        results.push(json);
    }
    let json = json::Json::object(vec![
        ("oracle", json::Json::str(&servers[0].name)),
        ("other", json::Json::str(&servers[1].name)),
        ("traces", json::Json::Array(results)),
    ]);
    write_result(&root, "replay", &json)?;
    Ok(total)
}

/// Runs `regress` or `isolation` and writes `run/results/<suite>.json`.
fn regress_command(suite: &regress::Suite, args: &[String]) -> Result<usize, String> {
    let mut options = regress::Options::default();
    for arg in args {
        match arg.as_str() {
            "--accept" => options.accept = true,
            other if other.starts_with("--") => return Err(format!("unknown option {other:?}")),
            test => options.tests.push(test.to_string()),
        }
    }
    let root = pins::root()?;
    let prefix = oracle::installed(&root, &pins::Pins::read(&root)?)?;
    let servers = servers::load(&root)?;
    let (count, json) = regress::run(&root, &prefix, suite, &servers[0], &servers[1], &options)?;
    write_result(&root, suite.name, &json)?;
    Ok(count)
}

/// Exit code 1 when something differs, so that a script can stop on it.
fn differences(count: usize) -> ExitCode {
    if count == 0 {
        ExitCode::SUCCESS
    } else {
        eprintln!("rudb-postgres: {count} cases differ");
        ExitCode::FAILURE
    }
}

fn up_options(args: &[String]) -> Result<servers::UpOptions, String> {
    let mut options = servers::UpOptions::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rudb" => {
                options.rudb = Some(PathBuf::from(args.next().ok_or("--rudb needs a binary")?))
            }
            "--port" => {
                let port = args.next().ok_or("--port needs a number")?;
                options.port = Some(port.parse().map_err(|_| format!("{port:?} is not a port"))?);
            }
            "--fresh" => options.fresh = true,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(options)
}
