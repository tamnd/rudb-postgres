//! The connect matrix: each way to log in, on both servers, compared.
//!
//! This is the smoke test of L1 in document 01 of the notes. Each case logs in to the oracle and
//! to the other server with the same parameters and compares what the server sent after the
//! startup message: the notices, the parameters, the key data and `ReadyForQuery`, or the error
//! of a refusal. A login that works then runs one query, so that the case also shows who the
//! server thinks the session is.

use crate::client::{Client, Login, Transport};
use crate::compare::{self, Difference};
use crate::json::Json;
use crate::servers::Server;

#[derive(Debug)]
struct Case {
    name: &'static str,
    login: Login,
}

fn cases() -> Vec<Case> {
    let tcp = |user: &str, password: &str, database: &str| Login {
        user: user.to_string(),
        password: password.to_string(),
        database: database.to_string(),
        transport: Transport::Tcp,
        options: Vec::new(),
    };
    let mut with_options = Login::rpg("postgres");
    with_options.options = vec![
        ("application_name".to_string(), "rudb-postgres".to_string()),
        ("options".to_string(), "-c search_path=pg_catalog -c DateStyle=German".to_string()),
    ];
    vec![
        Case { name: "trust over the socket", login: Login::superuser("postgres") },
        Case { name: "scram over tcp", login: Login::rpg("postgres") },
        Case { name: "md5 over tcp", login: tcp("rpg_md5", "rpg", "postgres") },
        Case { name: "password over tcp", login: tcp("rpg_password", "rpg", "postgres") },
        Case { name: "scram, wrong password", login: tcp("rpg", "wrong", "postgres") },
        Case { name: "md5, wrong password", login: tcp("rpg_md5", "wrong", "postgres") },
        Case { name: "scram, no such role", login: tcp("nobody", "rpg", "postgres") },
        Case { name: "no such database", login: Login::rpg("nowhere") },
        Case { name: "template0 refuses", login: Login::superuser("template0") },
        Case { name: "startup options", login: with_options },
    ]
}

/// Runs every case and prints one line per case. Returns the number of cases that differ, and
/// the result JSON.
pub(crate) fn run(oracle: &Server, other: &Server) -> (usize, Json) {
    println!("{:<26} {:<12} {:<12} result", "case", oracle.name, other.name);
    let mut differing = 0;
    let mut results = Vec::new();
    for case in cases() {
        let (a, a_frames) = attempt(oracle, &case.login);
        let (b, b_frames) = attempt(other, &case.login);
        let differences = compare::frames(&a_frames, &b_frames);
        let same = differences.is_empty() && a == b;
        println!(
            "{:<26} {:<12} {:<12} {}",
            case.name,
            a,
            b,
            if same { "same" } else { "DIFFERENT" }
        );
        results.push(Json::object(vec![
            ("case", Json::str(case.name)),
            ("oracle", Json::str(&a)),
            ("other", Json::str(&b)),
            ("same", Json::Bool(same)),
        ]));
        if !same {
            differing += 1;
            if a != b {
                println!("    result: {a} against {b}");
            }
            for Difference { at, what, oracle: x, other: y } in differences {
                println!(
                    "    frame {at}, {what}:\n      {}: {x}\n      {}: {y}",
                    oracle.name, other.name
                );
            }
        }
    }
    let json = Json::object(vec![
        ("oracle", Json::str(&oracle.name)),
        ("other", Json::str(&other.name)),
        ("cases", Json::Array(results)),
    ]);
    (differing, json)
}

/// Logs in and returns a short result and the frames to compare: the startup frames and the
/// reply to the query, or the frames of the refusal.
fn attempt(server: &Server, login: &Login) -> (String, Vec<crate::frame::Frame>) {
    match Client::connect(server, login) {
        Ok(mut client) => {
            let mut frames = std::mem::take(&mut client.startup);
            match client
                .simple("select current_user, session_user, current_setting('application_name')")
            {
                Ok(reply) => frames.extend(reply),
                Err(e) => return (format!("io: {e}"), frames),
            }
            ("ok".to_string(), frames)
        }
        Err(refused) => {
            let code = refused
                .frames
                .iter()
                .find(|f| f.tag == b'E')
                .and_then(|f| crate::frame::notice_fields(f).into_iter().find(|(c, _)| *c == b'C'))
                .map(|(_, code)| code);
            match code {
                Some(code) => (code, refused.frames),
                None => (format!("io: {}", refused.message), refused.frames),
            }
        }
    }
}
