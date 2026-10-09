//! The recording proxy of document 14 section 14.4 of the PostgreSQL compatibility notes.
//!
//! The proxy listens on a TCP port on the loopback address, or on a Unix socket. For each client
//! it opens a connection of the same kind to the server and forwards each frame in both
//! directions without a change. The regression runner of section 14.8 uses the Unix socket, so
//! that `pg_regress` and its psql sessions log in with trust as they do without the proxy. Before
//! it forwards a frame, it writes the line of the frame to the trace of the session. So the order
//! of the lines is an order that the session can have: a backend line comes after the frontend
//! line that caused it, and a frontend line comes after each backend line that the client could
//! read before it sent the frame.
//!
//! The proxy has no TLS. It answers `SSLRequest` and `GSSENCRequest` with `N`, as a server
//! without TLS does, so a client with `sslmode=prefer` continues without TLS. The cases for TLS
//! and channel binding run without the proxy.
//!
//! A client sends a `CancelRequest` on a new connection. The proxy writes it to the trace of the
//! session whose `BackendKeyData` has the same process ID and key, and not to a trace of its own,
//! so that the replay sees the cancel at the place in the session where the client sent it. A
//! cancel for a key that the proxy did not see gets its own trace, as before.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::frame::{self, Frame};
use crate::servers::Server;
use crate::trace::{Style, Tracer};

/// The default port of the proxy, near the ports of the two servers.
pub(crate) const DEFAULT_PORT: u16 = 55440;

#[derive(Debug)]
pub(crate) struct Options {
    pub(crate) port: u16,
    /// The traces go to `run/traces/<name>-<n>.trace`, with n from 1 for each session.
    pub(crate) name: String,
    /// Stop after this many sessions. Without it, the proxy runs until it is stopped.
    pub(crate) sessions: Option<usize>,
    pub(crate) style: Style,
}

impl Default for Options {
    fn default() -> Options {
        Options { port: DEFAULT_PORT, name: "session".into(), sessions: None, style: Style::Exact }
    }
}

/// Records over TCP in front of a server, in `run/traces`, until the proxy has recorded the
/// sessions of the options, or until it is stopped.
pub(crate) fn record(root: &Path, server: &Server, options: &Options) -> Result<(), String> {
    let dir = root.join("run").join("traces");
    let listen = Address::Tcp(options.port);
    let recorder = start(&dir, listen, Address::Tcp(server.port), options)?;
    println!(
        "proxy on 127.0.0.1:{} in front of {} on port {}, traces in {}",
        options.port,
        server.name,
        server.port,
        dir.display()
    );
    recorder.wait()
}

/// Where the proxy listens, or where the server listens.
#[derive(Debug, Clone)]
pub(crate) enum Address {
    /// A port on the loopback address.
    Tcp(u16),
    /// The path of a Unix socket.
    Unix(PathBuf),
}

impl Address {
    /// The Unix socket of a server, in the form of libpq: `.s.PGSQL.<port>` in its socket
    /// directory.
    pub(crate) fn socket(dir: &Path, port: u16) -> Address {
        Address::Unix(dir.join(format!(".s.PGSQL.{port}")))
    }

    fn connect(&self) -> io::Result<Conn> {
        match self {
            Address::Tcp(port) => {
                let stream = TcpStream::connect(("127.0.0.1", *port))?;
                stream.set_nodelay(true)?;
                Ok(Conn::Tcp(stream))
            }
            Address::Unix(path) => Ok(Conn::Unix(UnixStream::connect(path)?)),
        }
    }
}

/// A proxy that runs on its own thread and writes a trace for each session in a directory.
pub(crate) struct Recorder {
    listen: Address,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Result<(), String>>,
}

/// Starts a proxy that listens on `listen` and forwards each session to `server`.
pub(crate) fn start(
    dir: &Path,
    listen: Address,
    server: Address,
    options: &Options,
) -> Result<Recorder, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let listener = match &listen {
        Address::Tcp(port) => Listener::Tcp(
            TcpListener::bind(("127.0.0.1", *port))
                .map_err(|e| format!("cannot listen on port {port}: {e}"))?,
        ),
        Address::Unix(path) => Listener::Unix(
            UnixListener::bind(path)
                .map_err(|e| format!("cannot listen on {}: {e}", path.display()))?,
        ),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let (dir, stop) = (dir.to_owned(), Arc::clone(&stop));
        let (name, sessions, style) = (options.name.clone(), options.sessions, options.style);
        thread::spawn(move || {
            let keys: Keys = Arc::default();
            let mut threads = Vec::new();
            for i in 0.. {
                let client = listener.accept().map_err(|e| format!("cannot accept a client: {e}"));
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let client = client?;
                let path = dir.join(format!("{name}-{}.trace", i + 1));
                let (server, keys) = (server.clone(), Arc::clone(&keys));
                threads.push(thread::spawn(move || {
                    if let Err(error) = session(client, &server, &path, style, &keys) {
                        eprintln!("{}: {error}", path.display());
                    }
                }));
                if sessions == Some(i + 1) {
                    break;
                }
            }
            for thread in threads {
                let _ = thread.join();
            }
            Ok(())
        })
    };
    Ok(Recorder { listen, stop, thread })
}

impl Recorder {
    /// Waits until the proxy has recorded the sessions of its options.
    pub(crate) fn wait(self) -> Result<(), String> {
        self.thread.join().unwrap_or_else(|_| Err("the proxy stopped with a panic".into()))
    }

    /// Stops the proxy after the sessions that are open now end, and waits for them.
    pub(crate) fn stop(self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        // A connection wakes the thread that waits in accept, and it then sees the flag.
        let _ = self.listen.connect();
        let listen = self.listen.clone();
        let done = self.wait();
        if let Address::Unix(path) = listen {
            let _ = std::fs::remove_file(path);
        }
        done
    }
}

enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

impl Listener {
    fn accept(&self) -> io::Result<Conn> {
        match self {
            Listener::Tcp(l) => {
                let (stream, _) = l.accept()?;
                stream.set_nodelay(true)?;
                Ok(Conn::Tcp(stream))
            }
            Listener::Unix(l) => Ok(Conn::Unix(l.accept()?.0)),
        }
    }
}

/// One side of a session, over TCP or a Unix socket.
enum Conn {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Conn {
    fn try_clone(&self) -> io::Result<Conn> {
        match self {
            Conn::Tcp(s) => s.try_clone().map(Conn::Tcp),
            Conn::Unix(s) => s.try_clone().map(Conn::Unix),
        }
    }

    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self {
            Conn::Tcp(s) => s.shutdown(how),
            Conn::Unix(s) => s.shutdown(how),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.read(buf),
            Conn::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.write(buf),
            Conn::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Conn::Tcp(s) => s.flush(),
            Conn::Unix(s) => s.flush(),
        }
    }
}

/// The trace of one session. The two directions share it, so that the lines keep the order in
/// which the proxy forwarded the frames.
struct Trace {
    file: File,
    tracer: Tracer,
}

impl Trace {
    fn line(&mut self, line: &[u8]) -> io::Result<()> {
        let mut bytes = line.to_vec();
        bytes.push(b'\n');
        self.file.write_all(&bytes)
    }

    fn frame(&mut self, frontend: bool, frame: &Frame) -> io::Result<()> {
        let line = self.tracer.frame(frontend, frame);
        self.line(&line)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The trace of each live session, by the body of its `BackendKeyData`: the process ID and the
/// key, the same bytes that a `CancelRequest` carries after its code.
type Keys = Arc<Mutex<HashMap<Vec<u8>, Arc<Mutex<Trace>>>>>;

fn session(
    mut client: Conn,
    server: &Address,
    path: &PathBuf,
    style: Style,
    keys: &Keys,
) -> io::Result<()> {
    // The messages without a type byte. The client can ask for TLS or GSS encryption before
    // the startup, and the proxy refuses both. The lines wait in memory until the proxy knows
    // which trace they go to.
    let tracer = Tracer::new(style);
    let mut lines = Vec::new();
    let (first, code) = loop {
        let message = frame::read_untagged(&mut client)?;
        let code = u32::from_be_bytes([message[4], message[5], message[6], message[7]]);
        lines.push(tracer.untagged(&message));
        let name = match code {
            frame::SSL_REQUEST => "SSLResponse",
            frame::GSSENC_REQUEST => "GSSENCResponse",
            _ => break (message, code),
        };
        client.write_all(b"N")?;
        lines.push(tracer.char_response(name, b'N'));
    };

    let target =
        if code == frame::CANCEL_REQUEST { lock(keys).get(&first[8..]).cloned() } else { None };
    let trace = match target {
        Some(target) => {
            // Only the cancel goes to the session. The requests for TLS before it are part of
            // how this client connects, and not part of the session.
            let line = lines.pop().unwrap_or_default();
            lock(&target).line(&line)?;
            None
        }
        None => {
            let mut trace = Trace { file: File::create(path)?, tracer };
            for line in &lines {
                trace.line(line)?;
            }
            Some(Arc::new(Mutex::new(trace)))
        }
    };

    let mut server = server.connect()?;
    server.write_all(&first)?;
    let Some(trace) = trace.filter(|_| code != frame::CANCEL_REQUEST) else {
        // The server reads the key and closes the connection without an answer.
        return Ok(());
    };

    let backend = {
        let trace = Arc::clone(&trace);
        let keys = Arc::clone(keys);
        let mut from_server = server.try_clone()?;
        let mut to_client = client.try_clone()?;
        thread::spawn(move || -> io::Result<()> {
            let mut key = None;
            while let Ok(frame) = frame::read(&mut from_server) {
                lock(&trace).frame(false, &frame)?;
                if frame.tag == b'K' {
                    lock(&keys).insert(frame.body.clone(), Arc::clone(&trace));
                    key = Some(frame.body.clone());
                }
                if to_client.write_all(&frame.bytes()).is_err() {
                    break;
                }
            }
            if let Some(key) = key {
                lock(&keys).remove(&key);
            }
            let _ = to_client.shutdown(Shutdown::Both);
            Ok(())
        })
    };
    while let Ok(frame) = frame::read(&mut client) {
        lock(&trace).frame(true, &frame)?;
        if server.write_all(&frame.bytes()).is_err() {
            break;
        }
    }
    // The client is gone. The server ends the session when it reads the end of the stream.
    let _ = server.shutdown(Shutdown::Write);
    backend.join().unwrap_or(Ok(()))
}
