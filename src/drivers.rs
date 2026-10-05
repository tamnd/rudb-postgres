//! The driver gate of PG1: each client in `drivers/` connects over TLS with SCRAM and runs
//! `SELECT 1`, on both servers.
//!
//! This is gate 1 of document 17 section 17.3 of the notes. The scripts and the variables they
//! get are in `drivers/README.md`. A client passes on a server when its script ends with status 0
//! and the last line it prints is `1`.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::json::Json;
use crate::servers::Server;

/// The role of `oracle/setup.sql` that logs in with SCRAM over TCP.
const USER: &str = "rpg";
const PASSWORD: &str = "rpg";

/// The names of the clients, which are the directories of `drivers/` with a `run.sh`, in order.
pub(crate) fn names(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join("drivers");
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("run.sh").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

/// What one client did on one server.
struct Outcome {
    passed: bool,
    /// The last line of the output, or of the error output when the script failed.
    detail: String,
}

fn attempt(root: &Path, name: &str, oracle: &Server, server: &Server) -> Outcome {
    let dir = root.join("drivers").join(name);
    let output = Command::new("sh")
        .arg("run.sh")
        .current_dir(&dir)
        .env("GATE_HOST", "localhost")
        .env("GATE_PORT", server.port.to_string())
        .env("GATE_USER", USER)
        .env("GATE_PASSWORD", PASSWORD)
        .env("GATE_DATABASE", "postgres")
        .env("GATE_ROOT_CERT", root.join("oracle").join("certs").join("root.crt"))
        .env("GATE_PG_BIN", &oracle.binary)
        .env_remove("PGHOST")
        .env_remove("PGPORT")
        .env_remove("PGUSER")
        .env_remove("PGPASSWORD")
        .env_remove("PGSSLMODE")
        .stdin(Stdio::null())
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return Outcome { passed: false, detail: format!("cannot start sh: {error}") };
        }
    };
    let last = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map_or(String::new(), |line| line.trim().to_string())
    };
    let stdout = last(&output.stdout);
    if output.status.success() && stdout == "1" {
        return Outcome { passed: true, detail: stdout };
    }
    let detail = if output.status.success() { stdout } else { last(&output.stderr) };
    Outcome { passed: false, detail }
}

/// Runs each client on both servers and prints one line per client. Returns the number of
/// clients that fail on the other server, and the result JSON.
pub(crate) fn run(root: &Path, names: &[String], oracle: &Server, other: &Server) -> (usize, Json) {
    println!("{:<16} {:<8} {:<8} detail", "client", oracle.name, other.name);
    let mut failing = 0;
    let mut rows = Vec::new();
    for name in names {
        let a = attempt(root, name, oracle, oracle);
        let b = attempt(root, name, oracle, other);
        let mark = |o: &Outcome| if o.passed { "ok" } else { "FAIL" };
        let detail = if !b.passed {
            b.detail.clone()
        } else if !a.passed {
            a.detail.clone()
        } else {
            String::new()
        };
        println!("{:<16} {:<8} {:<8} {}", name, mark(&a), mark(&b), detail);
        if !b.passed {
            failing += 1;
        }
        rows.push(Json::object(vec![
            ("client", Json::str(name)),
            ("oracle", Json::Bool(a.passed)),
            ("other", Json::Bool(b.passed)),
            ("oracle_detail", Json::str(&a.detail)),
            ("other_detail", Json::str(&b.detail)),
        ]));
    }
    let json = Json::object(vec![
        ("other", Json::str(&other.name)),
        ("failing", Json::Number(failing as i64)),
        ("clients", Json::Array(rows)),
    ]);
    (failing, json)
}
