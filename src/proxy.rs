//! The recording proxy of document 14 section 14.4 of the PostgreSQL compatibility notes.
//!
//! The proxy listens on a TCP port on the loopback address. For each client it opens a TCP
//! connection to the server and forwards each frame in both directions without a change. Before
//! it forwards a frame, it writes the line of the frame to the trace of the session. So the order
//! of the lines is an order that the session can have: a backend line comes after the frontend
//! line that caused it, and a frontend line comes after each backend line that the client could
//! read before it sent the frame.
//!
//! The proxy has no TLS. It answers `SSLRequest` and `GSSENCRequest` with `N`, as a server
//! without TLS does, so a client with `sslmode=prefer` continues without TLS. The cases for TLS
//! and channel binding run without the proxy.

use std::fs::File;
use std::io::{self, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

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

pub(crate) fn record(root: &Path, server: &Server, options: &Options) -> Result<(), String> {
    let dir = root.join("run").join("traces");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let listener = TcpListener::bind(("127.0.0.1", options.port))
        .map_err(|e| format!("cannot listen on port {}: {e}", options.port))?;
    println!(
        "proxy on 127.0.0.1:{} in front of {} on port {}, traces in {}",
        options.port,
        server.name,
        server.port,
        dir.display()
    );
    let mut sessions = Vec::new();
    for (i, client) in listener.incoming().enumerate() {
        let client = client.map_err(|e| format!("cannot accept a client: {e}"))?;
        let path = dir.join(format!("{}-{}.trace", options.name, i + 1));
        let port = server.port;
        let style = options.style;
        sessions.push(thread::spawn(move || {
            if let Err(error) = session(client, port, &path, style) {
                eprintln!("{}: {error}", path.display());
            }
        }));
        if options.sessions == Some(i + 1) {
            break;
        }
    }
    for session in sessions {
        let _ = session.join();
    }
    Ok(())
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

fn lock(trace: &Mutex<Trace>) -> std::sync::MutexGuard<'_, Trace> {
    trace.lock().unwrap_or_else(|e| e.into_inner())
}

fn session(mut client: TcpStream, port: u16, path: &PathBuf, style: Style) -> io::Result<()> {
    let trace =
        Arc::new(Mutex::new(Trace { file: File::create(path)?, tracer: Tracer::new(style) }));
    // The messages without a type byte. The client can ask for TLS or GSS encryption before
    // the startup, and the proxy refuses both.
    let (first, code) = loop {
        let message = frame::read_untagged(&mut client)?;
        let code = u32::from_be_bytes([message[4], message[5], message[6], message[7]]);
        let mut t = lock(&trace);
        let line = t.tracer.untagged(&message);
        t.line(&line)?;
        let name = match code {
            frame::SSL_REQUEST => "SSLResponse",
            frame::GSSENC_REQUEST => "GSSENCResponse",
            _ => break (message, code),
        };
        client.write_all(b"N")?;
        let line = t.tracer.char_response(name, b'N');
        t.line(&line)?;
    };

    let mut server = TcpStream::connect(("127.0.0.1", port))?;
    server.set_nodelay(true)?;
    client.set_nodelay(true)?;
    server.write_all(&first)?;
    if code == frame::CANCEL_REQUEST {
        // The server reads the key and closes the connection without an answer.
        return Ok(());
    }

    let backend = {
        let trace = Arc::clone(&trace);
        let mut from_server = server.try_clone()?;
        let mut to_client = client.try_clone()?;
        thread::spawn(move || -> io::Result<()> {
            while let Ok(frame) = frame::read(&mut from_server) {
                lock(&trace).frame(false, &frame)?;
                if to_client.write_all(&frame.bytes()).is_err() {
                    break;
                }
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
