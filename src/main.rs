//! The command line of the PostgreSQL compatibility harness.
//!
//! Each subcommand prints its result and exits with a non-zero code when it finds a difference
//! that is not in an expected list. The design is document 14 of the PostgreSQL compatibility
//! notes, and the layout of this crate follows its section 14.2.

use std::process::ExitCode;

const USAGE: &str = "\
usage: rudb-postgres <command> [options]

commands:
  oracle build          build PostgreSQL from the pin into target/oracle/<pin>
  up [--rudb <binary>]  start the oracle and the other server, on two ports
  down                  stop the servers that up started
  record --to <server>  start the proxy in front of a server and write a trace per session
  replay <trace>        replay a trace against both servers and compare the replies
  diff <file.sql>       run each statement on both servers and compare the answers
  gen                   generate statements and run them through diff
  client <name>         run one client suite against both servers
  regress               run the core regression suite
  isolation             run the isolation specs
  report                write the report page of one run
";

/// The exit code for a command that is not written yet. It is not 1, so that a script can tell
/// a missing command from a difference.
const NOT_WRITTEN: u8 = 2;

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
        "oracle" | "up" | "down" | "record" | "replay" | "diff" | "gen" | "client" | "regress"
        | "isolation" | "report" => {
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
