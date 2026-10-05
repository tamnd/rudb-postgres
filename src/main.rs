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
mod frame;
mod oracle;
mod pins;
mod process;
mod scram;
mod servers;
mod toml;

const USAGE: &str = "\
usage: rudb-postgres <command> [options]

commands:
  oracle build          build PostgreSQL from the pin into target/oracle/<pin>
  up [--rudb <binary>] [--port <n>] [--fresh]
                        start the oracle on port n and the other server on n + 1; the
                        other server is rudb, or a second oracle without --rudb
  down                  stop the servers that up started
  connect               log in to both servers each way and compare the replies
  record --to <server>  start the proxy in front of a server and write a trace per session
  replay <trace>        replay a trace against both servers and compare the replies
  diff <file.sql>       run each statement on both servers and compare the answers
  gen                   generate statements and run them through diff
  client <name>         run one client suite against both servers
  regress               run the core regression suite
  isolation             run the isolation specs
  report                write the report page of one run
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
        "connect" => match pins::root().and_then(|root| servers::load(&root)) {
            Ok(servers) => differences(connect::run(&servers[0], &servers[1])),
            Err(error) => report(Err(error)),
        },
        "oracle" | "record" | "replay" | "diff" | "gen" | "client" | "regress" | "isolation"
        | "report" => {
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
