//! Start and stop the two servers of a run.
//!
//! `up` starts the oracle and one other server with the same configuration, on two ports, and
//! writes `run/servers.toml`. Every other command reads that file to find them. The other server
//! is rudb when `--rudb <binary>` is given. Without it, the other server is a second copy of the
//! oracle, the twin. A run of oracle against twin must show zero differences, because any
//! difference it shows comes from the harness, per document 17 section 17.2 of the notes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::oracle;
use crate::pins::Pins;
use crate::process;
use crate::toml::{self, Section};

/// The default port of the oracle. The other server is on the next port. Both are far from 5432,
/// so that a run does not meet a PostgreSQL that the machine already has.
pub(crate) const DEFAULT_PORT: u16 = 55432;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Postgres,
    Rudb,
}

#[derive(Debug, Clone)]
pub(crate) struct Server {
    /// `oracle`, `twin` or `rudb`.
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) port: u16,
    pub(crate) socket: PathBuf,
    pub(crate) data: PathBuf,
    /// The directory of the PostgreSQL programs, or the rudb binary.
    pub(crate) binary: PathBuf,
    /// The process ID of a rudb server. pg_ctl keeps its own.
    pub(crate) pid: Option<u32>,
}

#[derive(Debug, Default)]
pub(crate) struct UpOptions {
    pub(crate) rudb: Option<PathBuf>,
    pub(crate) port: Option<u16>,
    pub(crate) fresh: bool,
}

pub(crate) fn up(root: &Path, pins: &Pins, options: &UpOptions) -> Result<(), String> {
    let prefix = oracle::installed(root, pins)?;
    let bin = prefix.join("bin");
    let port = options.port.unwrap_or(DEFAULT_PORT);
    // A second `up` restarts what the first one started.
    if root.join("run").join("servers.toml").exists() {
        down(root)?;
    }

    let mut servers = vec![server(root, "oracle", Kind::Postgres, port, bin.clone())];
    servers.push(match &options.rudb {
        Some(binary) => {
            let binary =
                std::fs::canonicalize(binary).map_err(|e| format!("{}: {e}", binary.display()))?;
            server(root, "rudb", Kind::Rudb, port + 1, binary)
        }
        None => server(root, "twin", Kind::Postgres, port + 1, bin.clone()),
    });

    for server in &mut servers {
        let dir = server.data.parent().expect("a data directory has a parent").to_path_buf();
        if options.fresh && dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
        }
        std::fs::create_dir_all(&server.socket)
            .map_err(|e| format!("cannot create {}: {e}", server.socket.display()))?;
        let new = !server.data.exists();
        if new {
            init(root, server)?;
        }
        start(server)?;
        if new {
            process::run(psql(&bin, server).arg("-f").arg(root.join("oracle").join("setup.sql")))?;
        }
    }
    save(root, &servers)?;

    for server in &servers {
        let what = match (server.kind, server.name.as_str()) {
            (Kind::Postgres, "oracle") => {
                format!("PostgreSQL {} at {}", pins.postgres_describe, pins.postgres_short())
            }
            (Kind::Postgres, _) => "a second copy of the oracle".to_string(),
            (Kind::Rudb, _) => format!("rudb from {}", server.binary.display()),
        };
        println!(
            "{:<7} port {}  socket {}  {what}",
            server.name,
            server.port,
            server.socket.display()
        );
    }
    Ok(())
}

pub(crate) fn down(root: &Path) -> Result<(), String> {
    let file = root.join("run").join("servers.toml");
    if !file.exists() {
        println!("no servers are up");
        return Ok(());
    }
    for server in load(root)? {
        stop(&server)?;
        println!("{:<7} stopped", server.name);
    }
    std::fs::remove_file(&file).map_err(|e| format!("cannot remove {}: {e}", file.display()))
}

/// The oracle and the other server, in that order.
pub(crate) fn load(root: &Path) -> Result<Vec<Server>, String> {
    let file = root.join("run").join("servers.toml");
    let text = std::fs::read_to_string(&file)
        .map_err(|_| "no servers are up, run `rudb-postgres up` first".to_string())?;
    let mut servers = Vec::new();
    for section in toml::parse("run/servers.toml", &text)? {
        let get = |key: &str| section.require("run/servers.toml", key);
        servers.push(Server {
            name: section.name.clone(),
            kind: if get("kind")? == "rudb" { Kind::Rudb } else { Kind::Postgres },
            port: get("port")?
                .parse()
                .map_err(|_| format!("run/servers.toml: [{}] has a bad port", section.name))?,
            socket: PathBuf::from(get("socket")?),
            data: PathBuf::from(get("data")?),
            binary: PathBuf::from(get("binary")?),
            pid: section.get("pid").and_then(|p| p.parse().ok()),
        });
    }
    if servers.len() != 2 || servers[0].name != "oracle" {
        return Err("run/servers.toml must list the oracle and one other server".to_string());
    }
    Ok(servers)
}

fn server(root: &Path, name: &str, kind: Kind, port: u16, binary: PathBuf) -> Server {
    let dir = root.join("run").join(name);
    Server {
        name: name.to_string(),
        kind,
        port,
        socket: dir.join("sock"),
        data: dir.join("data"),
        binary,
        pid: None,
    }
}

/// Makes a data directory and gives it the shared configuration of `oracle/`.
fn init(root: &Path, server: &Server) -> Result<(), String> {
    match server.kind {
        Kind::Postgres => process::run(
            Command::new(server.binary.join("initdb")).arg("-D").arg(&server.data).args([
                "--no-locale",
                "--encoding=UTF8",
                "-U",
                "postgres",
                "-A",
                "trust",
                "--no-sync",
            ]),
        )?,
        // The same superuser as the oracle, so that `setup.sql` runs as `postgres` on both.
        Kind::Rudb => process::run(
            Command::new(&server.binary).arg("init").args(["-U", "postgres"]).arg(&server.data),
        )?,
    };
    let shared = root.join("oracle");
    copy(&shared.join("pg_hba.conf"), &server.data.join("pg_hba.conf"))?;
    for file in ["server.crt", "server.key", "root.crt"] {
        copy(&shared.join("certs").join(file), &server.data.join(file))?;
    }
    restrict(&server.data.join("server.key"))?;
    let conf = server.data.join("postgresql.conf");
    let mut text = std::fs::read_to_string(&conf).unwrap_or_default();
    text.push_str(&format!(
        "\n# Added by rudb-postgres up.\ninclude '{}'\n",
        shared.join("postgresql.conf").display()
    ));
    std::fs::write(&conf, text).map_err(|e| format!("cannot write {}: {e}", conf.display()))
}

fn start(server: &mut Server) -> Result<(), String> {
    let log = server.data.parent().expect("a data directory has a parent").join("server.log");
    match server.kind {
        Kind::Postgres => {
            let options = format!("-p {} -k {}", server.port, server.socket.display());
            process::run(
                Command::new(server.binary.join("pg_ctl"))
                    .arg("-D")
                    .arg(&server.data)
                    .arg("-l")
                    .arg(&log)
                    .args(["-o", &options, "-w", "-s", "start"]),
            )?;
        }
        Kind::Rudb => {
            // The same flags that `postgres` takes, per document 05 section 5.12 of the notes.
            let log = std::fs::File::create(&log)
                .map_err(|e| format!("cannot create {}: {e}", log.display()))?;
            let err = log.try_clone().map_err(|e| e.to_string())?;
            let mut child = Command::new(&server.binary)
                .arg("-D")
                .arg(&server.data)
                .arg("-p")
                .arg(server.port.to_string())
                .arg("-k")
                .arg(&server.socket)
                .stdin(Stdio::null())
                .stdout(log)
                .stderr(err)
                .spawn()
                .map_err(|e| format!("cannot start {}: {e}", server.binary.display()))?;
            server.pid = Some(child.id());
            let socket = server.socket.join(format!(".s.PGSQL.{}", server.port));
            let deadline = Instant::now() + Duration::from_secs(30);
            while !socket.exists() {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!(
                        "rudb exited with {status} before it listened, see its log"
                    ));
                }
                if Instant::now() > deadline {
                    return Err(format!(
                        "rudb did not listen on {} in 30 seconds",
                        socket.display()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Ok(())
}

fn stop(server: &Server) -> Result<(), String> {
    match server.kind {
        Kind::Postgres => {
            let running = Command::new(server.binary.join("pg_ctl"))
                .arg("-D")
                .arg(&server.data)
                .arg("status")
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if running {
                process::run(
                    Command::new(server.binary.join("pg_ctl"))
                        .arg("-D")
                        .arg(&server.data)
                        .args(["-m", "fast", "-w", "-s", "stop"]),
                )?;
            }
        }
        Kind::Rudb => {
            if let Some(pid) = server.pid {
                // SIGTERM is the smart shutdown of document 05 section 5.13 of the notes. The
                // process may be gone already, which is fine.
                let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
            }
        }
    }
    Ok(())
}

/// psql as the superuser over the Unix socket, where the rules allow trust.
pub(crate) fn psql(bin: &Path, server: &Server) -> Command {
    let mut command = Command::new(bin.join("psql"));
    command.args(["-X", "-q", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", "postgres", "-h"]);
    command.arg(&server.socket).arg("-p").arg(server.port.to_string());
    command
}

fn save(root: &Path, servers: &[Server]) -> Result<(), String> {
    let sections: Vec<Section> = servers
        .iter()
        .map(|server| {
            let mut section = Section::new(&server.name);
            section
                .set("kind", if server.kind == Kind::Rudb { "rudb" } else { "postgres" })
                .set("port", server.port.to_string())
                .set("socket", server.socket.display().to_string())
                .set("data", server.data.display().to_string())
                .set("binary", server.binary.display().to_string());
            if let Some(pid) = server.pid {
                section.set("pid", pid.to_string());
            }
            section
        })
        .collect();
    let file = root.join("run").join("servers.toml");
    std::fs::write(&file, toml::write(&sections))
        .map_err(|e| format!("cannot write {}: {e}", file.display()))
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

/// The server refuses a key that other users can read.
fn restrict(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot chmod {}: {e}", path.display()))
}
