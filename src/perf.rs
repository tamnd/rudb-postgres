//! The performance rows of PG1: the `SELECT 1` floor, the memory of an idle session and the
//! connection rate. The design is document 15 of the notes, sections 15.2, 15.11 and 15.12. Each
//! row runs the same client against both servers over the same transport, one server after the
//! other, so that the two runs do not share the CPU.

use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::client::{Client, Login, Transport};
use crate::frame::{self, Frame};
use crate::json::Json;
use crate::process;
use crate::servers::{Kind, Server};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    /// How long each timed row runs on each server.
    pub(crate) seconds: f64,
    /// How many idle sessions the memory row opens at most. The first measure is at one tenth of
    /// this number.
    pub(crate) idle: usize,
}

impl Default for Options {
    fn default() -> Options {
        // The shared configuration keeps the default `max_connections` of 100, and the harness
        // needs some of them for itself.
        Options { seconds: 3.0, idle: 90 }
    }
}

/// One measured row, with the number of each server.
#[derive(Debug)]
struct Row {
    name: String,
    unit: &'static str,
    /// True when a lower number is better, as for a latency.
    lower: bool,
    oracle: f64,
    other: f64,
}

impl Row {
    /// How many times better the other server is than the oracle.
    fn ratio(&self) -> f64 {
        if self.lower { self.oracle / self.other } else { self.other / self.oracle }
    }
}

/// Runs every row and prints the table. The JSON has the rows and the way memory was measured.
pub(crate) fn run(oracle: &Server, other: &Server, options: &Options) -> Result<Json, String> {
    let time = Duration::from_secs_f64(options.seconds);
    let mut rows = Vec::new();
    for (transport, name) in [(Transport::Unix, "unix"), (Transport::Tcp, "tcp")] {
        for extended in [false, true] {
            let flow = if extended { "extended" } else { "simple" };
            let measure = |server: &Server| floor(server, transport, extended, time);
            let (a, b) = (measure(oracle)?, measure(other)?);
            rows.push(Row {
                name: format!("SELECT 1, {name}, {flow}, median"),
                unit: "µs",
                lower: true,
                oracle: a.median,
                other: b.median,
            });
            rows.push(Row {
                name: format!("SELECT 1, {name}, {flow}, p99"),
                unit: "µs",
                lower: true,
                oracle: a.p99,
                other: b.p99,
            });
        }
    }
    for (login, name) in
        [(Login::superuser("postgres"), "trust"), (Login::rpg("postgres"), "scram")]
    {
        rows.push(Row {
            name: format!("connections per second, {name}"),
            unit: "/s",
            lower: false,
            oracle: rate(oracle, &login, time)?,
            other: rate(other, &login, time)?,
        });
    }
    let method = if cfg!(target_os = "linux") { "pss" } else { "rss" };
    rows.push(Row {
        name: format!("idle session, {method}"),
        unit: "KiB",
        lower: true,
        oracle: idle(oracle, options.idle)?,
        other: idle(other, options.idle)?,
    });
    print(&rows, &oracle.name, &other.name);
    let rows = rows
        .iter()
        .map(|row| {
            Json::object(vec![
                ("name", Json::str(&row.name)),
                ("unit", Json::str(row.unit)),
                ("oracle", Json::Float(row.oracle)),
                ("other", Json::Float(row.other)),
                ("ratio", Json::Float(row.ratio())),
            ])
        })
        .collect();
    Ok(Json::object(vec![
        ("other", Json::str(&other.name)),
        ("seconds", Json::Float(options.seconds)),
        ("memory", Json::str(method)),
        ("rows", Json::Array(rows)),
    ]))
}

fn print(rows: &[Row], oracle: &str, other: &str) {
    println!("{:<40} {:>12} {:>12} {:>8}", "row", oracle, other, "ratio");
    for row in rows {
        println!(
            "{:<40} {:>12} {:>12} {:>7.2}x",
            row.name,
            format!("{:.1} {}", row.oracle, row.unit),
            format!("{:.1} {}", row.other, row.unit),
            row.ratio()
        );
    }
}

#[derive(Debug)]
struct Latency {
    median: f64,
    p99: f64,
}

/// The round trip of `SELECT 1` on one session, in the simple flow or in the extended flow with
/// the unnamed statement, as libpq's `PQexecParams` sends it.
fn floor(
    server: &Server,
    transport: Transport,
    extended: bool,
    time: Duration,
) -> Result<Latency, String> {
    let login = match transport {
        Transport::Unix => Login::superuser("postgres"),
        Transport::Tcp => Login::rpg("postgres"),
    };
    let mut client = Client::connect(server, &login).map_err(|refused| refused.message)?;
    let frames: Vec<Frame> = if extended {
        vec![
            frame::parse("", "SELECT 1", &[]),
            frame::bind("", "", 0, &[], 0),
            frame::describe(b'P', ""),
            frame::execute("", 0),
            frame::sync(),
        ]
    } else {
        vec![frame::query("SELECT 1")]
    };
    let mut once = || -> Result<(), String> {
        client.send(&frames).map_err(|e| format!("{}: {e}", server.name))?;
        let reply = client.until_ready().map_err(|e| format!("{}: {e}", server.name))?;
        match reply.iter().find(|f| f.tag == b'E') {
            Some(error) => Err(format!("{}: {}", server.name, crate::client::error_text(error))),
            None => Ok(()),
        }
    };
    for _ in 0..1000 {
        once()?;
    }
    let mut samples = Vec::new();
    let start = Instant::now();
    while start.elapsed() < time {
        let before = Instant::now();
        once()?;
        samples.push(before.elapsed().as_secs_f64() * 1e6);
    }
    samples.sort_by(f64::total_cmp);
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    Ok(Latency { median: at(0.5), p99: at(0.99) })
}

/// Connections per second for a client that connects, runs `SELECT 1` and disconnects.
fn rate(server: &Server, login: &Login, time: Duration) -> Result<f64, String> {
    let mut count = 0u32;
    let start = Instant::now();
    while start.elapsed() < time {
        let mut client = Client::connect(server, login).map_err(|refused| refused.message)?;
        client.simple("SELECT 1").map_err(|e| format!("{}: {e}", server.name))?;
        drop(client);
        count += 1;
    }
    Ok(f64::from(count) / start.elapsed().as_secs_f64())
}

/// The memory of one idle session: the growth of the memory of the server between one tenth of
/// `most` sessions and `most` sessions, divided by the number of sessions between the two. The
/// difference removes the memory that does not depend on the number of sessions.
fn idle(server: &Server, most: usize) -> Result<f64, String> {
    let first = (most / 10).max(1);
    let login = Login::superuser("postgres");
    let mut clients = Vec::new();
    let open = |clients: &mut Vec<Client>, n: usize| -> Result<(), String> {
        while clients.len() < n {
            let mut client = Client::connect(server, &login).map_err(|refused| refused.message)?;
            client.simple("SELECT 1").map_err(|e| format!("{}: {e}", server.name))?;
            clients.push(client);
        }
        std::thread::sleep(Duration::from_millis(500));
        Ok(())
    };
    open(&mut clients, first)?;
    let low = memory(server)?;
    open(&mut clients, most)?;
    let high = memory(server)?;
    Ok((high - low) / (most - first) as f64)
}

/// The memory of the server in KiB: PSS on Linux and RSS elsewhere, summed over the postmaster
/// and every process under it for PostgreSQL.
fn memory(server: &Server) -> Result<f64, String> {
    let root = match server.kind {
        Kind::Rudb => server.pid.ok_or(format!("{}: no process ID", server.name))?,
        Kind::Postgres => {
            let file = server.data.join("postmaster.pid");
            let text = std::fs::read_to_string(&file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            text.lines().next().and_then(|line| line.trim().parse().ok()).ok_or(format!(
                "{}: no process ID in {}",
                server.name,
                file.display()
            ))?
        }
    };
    let table = process::run(Command::new("ps").args(["-A", "-o", "pid=,ppid=,rss="]))?;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut rss = HashMap::new();
    for line in table.lines() {
        let numbers: Vec<u64> =
            line.split_whitespace().filter_map(|word| word.parse().ok()).collect();
        if let [pid, ppid, kib] = numbers[..] {
            children.entry(ppid as u32).or_default().push(pid as u32);
            rss.insert(pid as u32, kib as f64);
        }
    }
    let mut total = 0.0;
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        total += pss(pid).unwrap_or_else(|| rss.get(&pid).copied().unwrap_or(0.0));
        stack.extend(children.get(&pid).into_iter().flatten());
    }
    Ok(total)
}

/// The PSS of a process in KiB, from `/proc`, which only Linux has.
fn pss(pid: u32) -> Option<f64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    let line = text.lines().find(|line| line.starts_with("Pss:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}
