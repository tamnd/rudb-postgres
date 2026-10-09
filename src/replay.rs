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
//! A client can read an OID from a reply and send it in a later query. The OIDs of user objects
//! are not the same on each server, or on one server from run to run. So the replay keeps a map
//! for each server from each recorded OID to the OID that the server gave in the same place. It
//! fills the map from the columns of type `oid` in text format, where the recorded value and the
//! value of the server are both user OIDs, 16384 or more. It writes the recorded OID in place of
//! the OID of the server in each `DataRow`, so that the comparison sees the same values on both
//! servers. And it writes the OID of the server in place of the recorded OID in each later
//! `Query`, `Parse` and text parameter of `Bind`.
//!
//! The proxy writes a `CancelRequest` in the trace of the session that it cancels. The replay
//! sends it on a new connection with the process ID and the key of its own session. It waits
//! first until `pg_stat_activity` shows the session as active, because a cancel that comes before
//! the query starts does nothing, and the replay must cancel the same query that the recording
//! did.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::client::{self, Client, Login, Transport};
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
    send: Vec<Send>,
    /// The backend lines of the recording.
    lines: Vec<Vec<u8>>,
}

/// One frontend line of a step.
enum Send {
    Frame(Frame),
    /// A `CancelRequest` for the session of the trace.
    Cancel,
}

/// The type of a column of type `oid`.
const OID: u32 = 26;
/// The first OID that PostgreSQL gives to a user object, `FirstNormalObjectId`.
const FIRST_USER_OID: u32 = 16384;

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
            for send in session.steps.get(*step).map_or(&[][..], |s| s.send.as_slice()) {
                match send {
                    Send::Frame(f) => {
                        println!("  {}", String::from_utf8_lossy(&tracer.frame(true, f)));
                    }
                    Send::Cancel => println!("  CancelRequest"),
                }
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
                let frame = if is_cancel(&bytes) {
                    Send::Cancel
                } else {
                    Send::Frame(frame::read(&mut bytes.as_slice()).map_err(|_| {
                        "a message without a type byte after the startup".to_string()
                    })?)
                };
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

pub(crate) fn is_startup(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[4..6] == [0, 3]
}

pub(crate) fn is_cancel(bytes: &[u8]) -> bool {
    bytes.len() >= 16 && bytes[4..8] == frame::CANCEL_REQUEST.to_be_bytes()
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
            return Err(format!("{}: {sql}: {}", server.name, client::error_text(error)));
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
    let mut oids = Oids::default();
    for step in &session.steps {
        let mut frames = Vec::new();
        for send in &step.send {
            match send {
                Send::Frame(f) => frames.push(oids.frontend(f)),
                Send::Cancel => {
                    if let Err(e) = client.send(&std::mem::take(&mut frames)) {
                        played.ended = Some(format!("the server closed the connection: {e}"));
                        break;
                    }
                    wait_until_active(server, client.pid);
                    if let Err(e) = client::cancel(server, client.pid, &client.key) {
                        played.ended = Some(format!("the cancel did not connect: {e}"));
                    }
                }
            }
        }
        if played.ended.is_some() {
            break;
        }
        if let Err(e) = client.send(&frames) {
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
        oids.backend(&mut frames, &step.lines);
        played.steps.push(frames);
        if played.ended.is_some() {
            break;
        }
    }
    Ok(played)
}

/// Waits until the session runs a statement, for at most `FRAME_TIMEOUT`. A server without
/// `pg_stat_activity` gets the cancel after the wait.
fn wait_until_active(server: &Server, pid: i32) {
    let Ok(mut admin) = Client::connect(server, &Login::superuser(DATABASE)) else { return };
    let sql = format!("select state from pg_catalog.pg_stat_activity where pid = {pid}");
    let start = Instant::now();
    while start.elapsed() < FRAME_TIMEOUT {
        match admin.simple(&sql) {
            Ok(frames) if frames.iter().any(|f| f.tag == b'E') => return,
            Ok(frames) if client::rows(&frames).first() == Some(&vec![Some("active".into())]) => {
                return;
            }
            Ok(_) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return,
        }
    }
}

/// The map from each recorded user OID to the OID of the server, for one session.
#[derive(Default)]
pub(crate) struct Oids {
    map: HashMap<u32, u32>,
    /// The type of each column of the last `RowDescription`, or 0 for a column in binary format.
    columns: Vec<u32>,
}

impl Oids {
    /// The frame to send: the recorded frame with the OIDs of the server.
    pub(crate) fn frontend(&self, f: &Frame) -> Frame {
        if self.map.is_empty() {
            return f.clone();
        }
        let mut fields = f.fields();
        let body = match f.tag {
            b'Q' => {
                let sql = fields.cstr().unwrap_or_default();
                let mut body = self.rewrite(sql);
                body.push(0);
                body
            }
            b'P' => {
                let name = fields.cstr().unwrap_or_default();
                let sql = fields.cstr().unwrap_or_default();
                let mut body = name.to_vec();
                body.push(0);
                body.extend(self.rewrite(sql));
                body.push(0);
                body.extend_from_slice(fields.rest());
                body
            }
            b'B' => match self.bind(f) {
                Some(body) => body,
                None => return f.clone(),
            },
            _ => return f.clone(),
        };
        Frame::new(f.tag, body)
    }

    /// A `Bind` with the OIDs of the server in each parameter in text format.
    fn bind(&self, f: &Frame) -> Option<Vec<u8>> {
        let mut fields = f.fields();
        let mut body = Vec::new();
        for _ in 0..2 {
            body.extend_from_slice(fields.cstr()?);
            body.push(0);
        }
        let formats: Vec<i16> = (0..fields.i16()?).map(|_| fields.i16()).collect::<Option<_>>()?;
        body.extend_from_slice(&(formats.len() as i16).to_be_bytes());
        for format in &formats {
            body.extend_from_slice(&format.to_be_bytes());
        }
        let count = fields.i16()?;
        body.extend_from_slice(&count.to_be_bytes());
        for i in 0..count as usize {
            let length = fields.i32()?;
            if length < 0 {
                body.extend_from_slice(&length.to_be_bytes());
                continue;
            }
            let value = fields.take(length as usize)?;
            let format = match formats.len() {
                0 => 0,
                1 => formats[0],
                _ => *formats.get(i)?,
            };
            let value = if format == 0 { self.rewrite(value) } else { value.to_vec() };
            body.extend_from_slice(&(value.len() as i32).to_be_bytes());
            body.extend(value);
        }
        body.extend_from_slice(fields.rest());
        Some(body)
    }

    /// The text with each number that is a recorded OID in the map replaced. A number is a run
    /// of digits with no letter, digit or underscore next to it.
    fn rewrite(&self, text: &[u8]) -> Vec<u8> {
        let word = |b: &u8| b.is_ascii_alphanumeric() || *b == b'_';
        let mut out = Vec::with_capacity(text.len());
        let mut i = 0;
        while i < text.len() {
            let end = i + text[i..].iter().take_while(|b| b.is_ascii_digit()).count();
            if end == i {
                out.push(text[i]);
                i += 1;
                continue;
            }
            let alone = (i == 0 || !word(&text[i - 1])) && text.get(end).is_none_or(|b| !word(b));
            let live = std::str::from_utf8(&text[i..end])
                .ok()
                .and_then(|n| n.parse::<u32>().ok())
                .and_then(|n| self.map.get(&n));
            match live {
                Some(live) if alone => out.extend_from_slice(live.to_string().as_bytes()),
                _ => out.extend_from_slice(&text[i..end]),
            }
            i = end;
        }
        out
    }

    /// Learns the OIDs of the frames that the server sent for a step, and writes the recorded
    /// OIDs in their place. The k-th `DataRow` of the step goes with the k-th recorded one.
    pub(crate) fn backend(&mut self, frames: &mut [Frame], lines: &[Vec<u8>]) {
        let mut recorded = lines.iter().filter_map(|l| trace::data_row(l));
        for f in frames.iter_mut() {
            match f.tag {
                b'T' => self.columns = columns(f),
                b'D' => {
                    let Some(want) = recorded.next() else { continue };
                    if let Some(body) = self.row(f, &want) {
                        f.body = body;
                    }
                }
                _ => {}
            }
        }
    }

    fn row(&mut self, f: &Frame, recorded: &[Option<Vec<u8>>]) -> Option<Vec<u8>> {
        let mut fields = f.fields();
        let count = fields.i16()?;
        let mut body = count.to_be_bytes().to_vec();
        let mut changed = false;
        for i in 0..count as usize {
            let length = fields.i32()?;
            if length < 0 {
                body.extend_from_slice(&length.to_be_bytes());
                continue;
            }
            let value = fields.take(length as usize)?;
            let number = |v: &[u8]| std::str::from_utf8(v).ok()?.parse::<u32>().ok();
            let pair = (self.columns.get(i) == Some(&OID))
                .then(|| Some((number(recorded.get(i)?.as_deref()?)?, number(value)?)))
                .flatten()
                .filter(|(was, now)| *was >= FIRST_USER_OID && *now >= FIRST_USER_OID);
            let value = match pair {
                Some((was, now)) => {
                    self.map.insert(was, now);
                    changed |= was != now;
                    was.to_string().into_bytes()
                }
                None => value.to_vec(),
            };
            body.extend_from_slice(&(value.len() as i32).to_be_bytes());
            body.extend(value);
        }
        changed.then_some(body)
    }
}

/// The type of each column of a `RowDescription`, or 0 for a column in binary format.
fn columns(f: &Frame) -> Vec<u32> {
    let mut fields = f.fields();
    let mut out = Vec::new();
    for _ in 0..fields.i16().unwrap_or(0) {
        let (Some(_), Some(_), Some(_), Some(oid), Some(_), Some(_), Some(format)) = (
            fields.cstr(),
            fields.i32(),
            fields.i16(),
            fields.i32(),
            fields.i16(),
            fields.i32(),
            fields.i16(),
        ) else {
            break;
        };
        out.push(if format == 0 { oid as u32 } else { 0 });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Oids, Send, session};
    use crate::frame::Frame;
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

    #[test]
    fn a_cancel_line_is_a_cancel_step() {
        let text = b"\
F\t41\tStartupMessage\t 3 0 \"user\" \"postgres\" \"database\" \"postgres\"
B\t5\tReadyForQuery\t I
F\t23\tQuery\t \"select pg_sleep(5)\"
F\t16\tCancelRequest\t 1234 5678 65247 'Z\\xb4\\x17s'
B\t5\tReadyForQuery\t I
";
        let s = session(read(text).unwrap()).unwrap();
        assert_eq!(s.steps.len(), 1);
        assert!(matches!(s.steps[0].send.as_slice(), [Send::Frame(_), Send::Cancel]));
    }

    #[test]
    fn a_recorded_oid_becomes_the_oid_of_the_server() {
        let mut oids = Oids::default();
        oids.map.insert(16384, 16999);
        let sql = b"select 1 where c.oid = '16384' and t16384 = 163840 and 16384=x\0";
        let f = oids.frontend(&Frame::new(b'Q', sql.to_vec()));
        assert_eq!(f.body, b"select 1 where c.oid = '16999' and t16384 = 163840 and 16999=x\0");
    }

    #[test]
    fn an_oid_column_fills_the_map_and_shows_the_recorded_oid() {
        let mut oids = Oids { columns: vec![26, 23], ..Oids::default() };
        let mut body = 2i16.to_be_bytes().to_vec();
        for v in [&b"16999"[..], b"16999"] {
            body.extend_from_slice(&(v.len() as i32).to_be_bytes());
            body.extend_from_slice(v);
        }
        let mut frames = [Frame::new(b'D', body)];
        let lines = [b"B\t24\tDataRow\t 2 5 '16384' 5 '16384'".to_vec()];
        oids.backend(&mut frames, &lines);
        assert_eq!(oids.map.get(&16384), Some(&16999));
        // The int4 column is not an OID, so it keeps the value of the server.
        assert_eq!(&frames[0].body[6..11], b"16384");
        assert_eq!(&frames[0].body[15..20], b"16999");
    }
}
