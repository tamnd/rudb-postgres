//! The replay of a trace against two servers, per document 14 section 14.4 of the notes.
//!
//! The replay reads a trace in the exact style of [`crate::trace`]. A recorded SCRAM proof is
//! not valid a second time, so the replay does its own login with the user and the other
//! parameters of the recorded startup message, and starts after the first
//! `ReadyForQuery`. Each run of frontend lines is one step: the replay sends the frames of the
//! step as recorded, `CopyData` boundaries too, and reads as many frames as the trace has backend
//! lines after them.
//!
//! The replay compares the frames of the two servers step by step under the rules of document
//! 01 section 1.5. It stops at the first step that differs, because after that point the
//! recorded frontend frames are not what a client would send to the other server.
//!
//! Each trace runs in a new database `replay`, made from `template0` on both servers, and not in
//! the database of the recording. So a trace does not see the objects that an earlier trace or
//! the recording left behind.
//!
//! The OID map and the new cancel key of section 14.4 are not done yet. A trace that needs them
//! differs at the step where it sends a recorded OID or key.

use std::path::Path;
use std::time::Duration;

use crate::client::{Client, Login, Transport};
use crate::compare;
use crate::frame::{self, Frame};
use crate::json::Json;
use crate::servers::Server;
use crate::trace::{self, Line, Style, Tracer};

/// How long the replay waits for one frame. A server that sends fewer frames than the trace has
/// makes the replay wait this long once, at the end of its session.
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);

struct Session {
    user: String,
    options: Vec<(String, String)>,
    steps: Vec<Step>,
}

struct Step {
    send: Vec<Frame>,
    /// The backend lines of the recording.
    lines: Vec<Vec<u8>>,
}

/// What one server sent for each step. A session that ended early has fewer steps, and the
/// reason.
struct Played {
    steps: Vec<Vec<Frame>>,
    ended: Option<String>,
}

/// Replays one trace on both servers and returns the number of differences, 0 or 1, and the
/// result JSON.
pub(crate) fn run(path: &Path, oracle: &Server, other: &Server) -> Result<(usize, Json), String> {
    let text = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let session = session(trace::read(&text)?).map_err(|e| format!("{}: {e}", path.display()))?;
    let a = play(oracle, &session).map_err(|e| format!("the oracle refused the login: {e}"))?;
    let b = play(other, &session).unwrap_or_else(|e| Played { steps: Vec::new(), ended: Some(e) });

    let mut difference = None;
    for i in 0..session.steps.len() {
        let x = a.steps.get(i).map_or(&[][..], Vec::as_slice);
        let y = b.steps.get(i).map_or(&[][..], Vec::as_slice);
        if let Some(d) = compare::frames(x, y).into_iter().next() {
            difference = Some((i, d));
            break;
        }
    }
    if difference.is_none() && a.ended != b.ended {
        let at = a.steps.len().min(b.steps.len());
        difference = Some((
            at,
            compare::Difference {
                at: 0,
                what: "the session ended".into(),
                oracle: a.ended.clone().unwrap_or_else(|| "it did not end".into()),
                other: b.ended.clone().unwrap_or_else(|| "it did not end".into()),
            },
        ));
    }

    let name = path
        .file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    match &difference {
        None => println!(
            "{name}: {} steps, {} and {} give the same frames",
            session.steps.len(),
            oracle.name,
            other.name
        ),
        Some((step, d)) => {
            println!("{name}: step {} of {} differs: {}", step + 1, session.steps.len(), d.what);
            let mut tracer = Tracer::new(Style::Exact);
            for f in session.steps.get(*step).map_or(&[][..], |s| s.send.as_slice()) {
                println!("  {}", String::from_utf8_lossy(&tracer.frame(true, f)));
            }
            println!("  {}: {}", oracle.name, d.oracle);
            println!("  {}: {}", other.name, d.other);
        }
    }

    let json = Json::object(vec![
        ("trace", Json::str(&name)),
        ("steps", Json::Number(session.steps.len() as i64)),
        (
            "difference",
            match &difference {
                None => Json::Bool(false),
                Some((step, d)) => Json::object(vec![
                    ("step", Json::Number(*step as i64 + 1)),
                    ("what", Json::str(&d.what)),
                    ("oracle", Json::str(&d.oracle)),
                    ("other", Json::str(&d.other)),
                ]),
            },
        ),
    ]);
    Ok((usize::from(difference.is_some()), json))
}

/// Finds the startup message and the steps after the first `ReadyForQuery`.
fn session(lines: Vec<Line>) -> Result<Session, String> {
    let mut startup = None;
    let mut ready = false;
    let mut steps: Vec<Step> = Vec::new();
    for line in lines {
        match line {
            Line::Frontend(bytes) if !ready => {
                if startup.is_none() && is_startup(&bytes) {
                    startup = Some(bytes);
                }
            }
            Line::Backend(line) if !ready => {
                ready = line.starts_with(b"B\t5\tReadyForQuery\t");
            }
            Line::Frontend(bytes) => {
                let frame = frame::read(&mut bytes.as_slice())
                    .map_err(|_| "a message without a type byte after the startup".to_string())?;
                match steps.last_mut() {
                    Some(step) if step.lines.is_empty() => step.send.push(frame),
                    _ => steps.push(Step { send: vec![frame], lines: Vec::new() }),
                }
            }
            Line::Backend(line) => match steps.last_mut() {
                Some(step) => step.lines.push(line),
                None => {
                    return Err(
                        "a backend line after ReadyForQuery and before any frontend line".into()
                    );
                }
            },
        }
    }
    let startup = startup.ok_or("the trace has no StartupMessage")?;
    if !ready {
        return Err("the trace has no ReadyForQuery".into());
    }
    let mut fields =
        startup[8..].split(|b| *b == 0).map(|f| String::from_utf8_lossy(f).into_owned());
    let mut user = String::new();
    let mut options = Vec::new();
    while let (Some(name), Some(value)) = (fields.next(), fields.next()) {
        if name.is_empty() {
            break;
        }
        match name.as_str() {
            "user" => user = value,
            "database" => {}
            _ => options.push((name, value)),
        }
    }
    Ok(Session { user, options, steps })
}

fn is_startup(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[4..6] == [0, 3]
}

/// The passwords of the roles of `oracle/setup.sql`. They are test values.
fn password(user: &str) -> &'static str {
    match user {
        "postgres" => "postgres",
        "rpg" | "rpg_md5" | "rpg_password" => "rpg",
        _ => "",
    }
}

/// The database that each replay runs in.
const DATABASE: &str = "replay";

/// Drops and makes the database of the replay, as the superuser over the Unix socket.
fn fresh(server: &Server) -> Result<(), String> {
    let mut admin =
        Client::connect(server, &Login::superuser("postgres")).map_err(|r| r.message)?;
    for sql in [
        format!("DROP DATABASE IF EXISTS {DATABASE} WITH (FORCE)"),
        format!("CREATE DATABASE {DATABASE} TEMPLATE template0"),
    ] {
        let frames = admin.simple(&sql).map_err(|e| format!("{}: {e}", server.name))?;
        if let Some(error) = frames.iter().find(|f| f.tag == b'E') {
            return Err(format!("{}: {sql}: {}", server.name, crate::client::error_text(error)));
        }
    }
    Ok(())
}

fn play(server: &Server, session: &Session) -> Result<Played, String> {
    fresh(server)?;
    let login = Login {
        user: session.user.clone(),
        password: password(&session.user).to_string(),
        database: DATABASE.to_string(),
        transport: Transport::Tcp,
        options: session.options.clone(),
    };
    let mut client = Client::connect(server, &login).map_err(|r| r.message)?;
    client.set_timeout(FRAME_TIMEOUT).map_err(|e| e.to_string())?;
    let mut played = Played { steps: Vec::new(), ended: None };
    for step in &session.steps {
        if let Err(e) = client.send(&step.send) {
            played.ended = Some(format!("the server closed the connection: {e}"));
            break;
        }
        let mut frames = Vec::new();
        for _ in 0..step.lines.len() {
            match client.read() {
                Ok(frame) => frames.push(frame),
                Err(e) => {
                    played.ended = Some(match e.kind() {
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                            format!("no frame after {} s", FRAME_TIMEOUT.as_secs())
                        }
                        _ => "the server closed the connection".to_string(),
                    });
                    break;
                }
            }
        }
        played.steps.push(frames);
        if played.ended.is_some() {
            break;
        }
    }
    Ok(played)
}

#[cfg(test)]
mod tests {
    use super::session;
    use crate::trace::read;

    #[test]
    fn the_steps_start_after_the_first_ready_for_query() {
        let text = b"\
F\t8\tSSLRequest\t 1234 5679
B\t1\tSSLResponse\t N
F\t41\tStartupMessage\t 3 0 \"user\" \"postgres\" \"database\" \"postgres\"
B\t8\tAuthenticationOk
B\t5\tReadyForQuery\t I
F\t13\tQuery\t \"select 1\"
B\t13\tCommandComplete\t \"SELECT 1\"
B\t5\tReadyForQuery\t I
F\t6\tQuery\t \"x\"
F\t4\tSync
B\t5\tReadyForQuery\t I
F\t4\tTerminate
";
        let s = session(read(text).unwrap()).unwrap();
        assert_eq!(s.user, "postgres");
        assert_eq!(s.steps.len(), 3);
        assert_eq!((s.steps[0].send.len(), s.steps[0].lines.len()), (1, 2));
        assert_eq!((s.steps[1].send.len(), s.steps[1].lines.len()), (2, 1));
        assert_eq!((s.steps[2].send.len(), s.steps[2].lines.len()), (1, 0));
    }
}
