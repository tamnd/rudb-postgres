//! A protocol client over the frame layer.
//!
//! The differential runner uses this and not a driver, because a driver turns bytes into values
//! and that hides differences in the bytes. The client logs in with trust, password, md5 or
//! SCRAM-SHA-256, keeps the frames the server sent during startup, and then sends frames and
//! reads frames. It has no TLS. The TLS cases run through libpq.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::crypto::{hex, md5};
use crate::frame::{self, Frame};
use crate::scram::Scram;
use crate::servers::Server;

/// How long the client waits for one frame. A step that waits longer than this has hung, and
/// the isolation runner uses the same limit, per document 14 section 14.9 of the notes.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transport {
    Tcp,
    Unix,
}

#[derive(Debug, Clone)]
pub(crate) struct Login {
    pub(crate) user: String,
    pub(crate) password: String,
    pub(crate) database: String,
    pub(crate) transport: Transport,
    /// More startup parameters, for example `application_name`.
    pub(crate) options: Vec<(String, String)>,
}

impl Login {
    /// The normal user over TCP, so that the login runs SCRAM.
    pub(crate) fn rpg(database: &str) -> Login {
        Login {
            user: "rpg".to_string(),
            password: "rpg".to_string(),
            database: database.to_string(),
            transport: Transport::Tcp,
            options: Vec::new(),
        }
    }

    /// The superuser over the Unix socket, where the rules allow trust.
    pub(crate) fn superuser(database: &str) -> Login {
        Login {
            user: "postgres".to_string(),
            password: String::new(),
            database: database.to_string(),
            transport: Transport::Unix,
            options: Vec::new(),
        }
    }
}

/// A login that did not end in `ReadyForQuery`. The frames are what the server sent after the
/// startup message, so that a refusal can be compared like any other answer.
#[derive(Debug)]
pub(crate) struct Refused {
    pub(crate) message: String,
    pub(crate) frames: Vec<Frame>,
}

impl From<String> for Refused {
    fn from(message: String) -> Refused {
        Refused { message, frames: Vec::new() }
    }
}

#[derive(Debug)]
enum Stream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            Stream::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(buf),
            Stream::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.flush(),
            Stream::Unix(s) => s.flush(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Client {
    stream: Stream,
    /// Every frame after authentication, up to and including the first `ReadyForQuery`.
    pub(crate) startup: Vec<Frame>,
    /// The process ID from `BackendKeyData`.
    pub(crate) pid: i32,
}

impl Client {
    pub(crate) fn connect(server: &Server, login: &Login) -> Result<Client, Refused> {
        let stream = match login.transport {
            Transport::Tcp => {
                let s = TcpStream::connect(("127.0.0.1", server.port)).map_err(|e| {
                    format!("{}: cannot connect to port {}: {e}", server.name, server.port)
                })?;
                s.set_nodelay(true).map_err(|e| e.to_string())?;
                s.set_read_timeout(Some(READ_TIMEOUT)).map_err(|e| e.to_string())?;
                Stream::Tcp(s)
            }
            Transport::Unix => {
                let path = server.socket.join(format!(".s.PGSQL.{}", server.port));
                let s = UnixStream::connect(&path).map_err(|e| {
                    format!("{}: cannot connect to {}: {e}", server.name, path.display())
                })?;
                s.set_read_timeout(Some(READ_TIMEOUT)).map_err(|e| e.to_string())?;
                Stream::Unix(s)
            }
        };
        let mut client = Client { stream, startup: Vec::new(), pid: 0 };
        match client.login(login) {
            Ok(()) => Ok(client),
            Err(message) => Err(Refused {
                message: format!("{}: {message}", server.name),
                frames: std::mem::take(&mut client.startup),
            }),
        }
    }

    /// Logs in. Every frame after the startup message that is not an authentication request goes
    /// in `self.startup`, the error too, so that a refusal keeps its frames.
    fn login(&mut self, login: &Login) -> Result<(), String> {
        let mut parameters =
            vec![("user", login.user.as_str()), ("database", login.database.as_str())];
        for (name, value) in &login.options {
            parameters.push((name, value));
        }
        let io = |e: io::Error| e.to_string();
        self.stream.write_all(&frame::startup(frame::PROTOCOL_3_0, &parameters)).map_err(io)?;
        let mut scram: Option<Scram> = None;
        loop {
            let reply = frame::read(&mut self.stream).map_err(io)?;
            match reply.tag {
                b'R' => {
                    let mut fields = reply.fields();
                    match fields.i32() {
                        Some(0) => break,
                        Some(3) => {
                            let mut body = login.password.as_bytes().to_vec();
                            body.push(0);
                            self.send(&[frame::password(&body)]).map_err(io)?;
                        }
                        Some(5) => {
                            let salt = fields.take(4).ok_or("an md5 request without a salt")?;
                            let inner =
                                hex(&md5(format!("{}{}", login.password, login.user).as_bytes()));
                            let mut outer = inner.into_bytes();
                            outer.extend_from_slice(salt);
                            let mut body = format!("md5{}", hex(&md5(&outer))).into_bytes();
                            body.push(0);
                            self.send(&[frame::password(&body)]).map_err(io)?;
                        }
                        Some(10) => {
                            let mut mechanisms = Vec::new();
                            while let Some(m) = fields.cstr().filter(|m| !m.is_empty()) {
                                mechanisms.push(String::from_utf8_lossy(m).into_owned());
                            }
                            if !mechanisms.iter().any(|m| m == "SCRAM-SHA-256") {
                                return Err(format!(
                                    "the server offers {mechanisms:?} and not SCRAM-SHA-256"
                                ));
                            }
                            let state = Scram::new(&login.password, &nonce());
                            let first = state.client_first();
                            self.send(&[frame::sasl_initial("SCRAM-SHA-256", first.as_bytes())])
                                .map_err(io)?;
                            scram = Some(state);
                        }
                        Some(11) => {
                            let state = scram.as_mut().ok_or("SASLContinue before SASL")?;
                            let server_first = String::from_utf8_lossy(fields.rest()).into_owned();
                            let last = state.client_final(&server_first)?;
                            self.send(&[frame::password(last.as_bytes())]).map_err(io)?;
                        }
                        Some(12) => {
                            let state = scram.as_ref().ok_or("SASLFinal before SASL")?;
                            state.verify(&String::from_utf8_lossy(fields.rest()))?;
                        }
                        other => {
                            return Err(format!(
                                "an authentication request the harness does not do: {other:?}"
                            ));
                        }
                    }
                }
                b'E' => {
                    let text = error_text(&reply);
                    self.startup.push(reply);
                    return Err(text);
                }
                b'N' => self.startup.push(reply),
                other => {
                    return Err(format!("{:?} during authentication", frame::backend_name(other)));
                }
            }
        }
        // After AuthenticationOk the server can still refuse, for example when the database does
        // not exist, and then it closes the connection without ReadyForQuery.
        loop {
            let f = self.read().map_err(io)?;
            let tag = f.tag;
            if tag == b'K' {
                self.pid = f.fields().i32().unwrap_or(0);
            }
            let text = (tag == b'E').then(|| error_text(&f));
            self.startup.push(f);
            if let Some(text) = text {
                return Err(text);
            }
            if tag == b'Z' {
                return Ok(());
            }
        }
    }

    pub(crate) fn send(&mut self, frames: &[Frame]) -> io::Result<()> {
        let mut bytes = Vec::new();
        for f in frames {
            bytes.extend_from_slice(&f.bytes());
        }
        self.stream.write_all(&bytes)?;
        self.stream.flush()
    }

    /// Changes how long the client waits for one frame.
    pub(crate) fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        match &self.stream {
            Stream::Tcp(s) => s.set_read_timeout(Some(timeout)),
            Stream::Unix(s) => s.set_read_timeout(Some(timeout)),
        }
    }

    pub(crate) fn read(&mut self) -> io::Result<Frame> {
        frame::read(&mut self.stream)
    }

    /// Reads frames up to and including the next `ReadyForQuery`.
    pub(crate) fn until_ready(&mut self) -> io::Result<Vec<Frame>> {
        let mut frames = Vec::new();
        loop {
            let f = self.read()?;
            let done = f.tag == b'Z';
            frames.push(f);
            if done {
                return Ok(frames);
            }
        }
    }

    /// Sends a `Query` and reads the reply.
    pub(crate) fn simple(&mut self, sql: &str) -> io::Result<Vec<Frame>> {
        self.send(&[frame::query(sql)])?;
        self.until_ready()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.send(&[frame::terminate()]);
    }
}

/// The severity, code and message of an error, for a person to read.
pub(crate) fn error_text(f: &Frame) -> String {
    let fields = frame::notice_fields(f);
    let get =
        |code: u8| fields.iter().find(|(c, _)| *c == code).map(|(_, v)| v.as_str()).unwrap_or("");
    format!("{} {}: {}", get(b'S'), get(b'C'), get(b'M'))
}

/// The text columns of each `DataRow` in a reply, with `None` for a NULL.
pub(crate) fn rows(frames: &[Frame]) -> Vec<Vec<Option<String>>> {
    let mut rows = Vec::new();
    for f in frames.iter().filter(|f| f.tag == b'D') {
        let mut fields = f.fields();
        let count = fields.i16().unwrap_or(0);
        let mut row = Vec::new();
        for _ in 0..count {
            match fields.i32() {
                Some(n) if n >= 0 => row
                    .push(fields.take(n as usize).map(|b| String::from_utf8_lossy(b).into_owned())),
                _ => row.push(None),
            }
        }
        rows.push(row);
    }
    rows
}

/// A SCRAM nonce from the system's random source.
fn nonce() -> String {
    let mut bytes = [0u8; 18];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_err() {
        // A nonce only has to be fresh for one login against a test server.
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        bytes[..16].copy_from_slice(&now.as_nanos().to_be_bytes());
    }
    crate::crypto::base64_encode(&bytes)
}
