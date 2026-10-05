//! The upstream client suites of document 14 section 14.6 of the notes.
//!
//! Each client has a directory `clients/<name>` with these files:
//!
//! - `client.toml`: a `[client]` section with the `repository`, the `tag` and its `commit`, and
//!   the `format` of the raw results, `go-json` or `junit`. `strip` is an optional prefix that
//!   the harness removes from each test name.
//! - `run.sh`: runs the suite in the checkout, which is its working directory, and writes the
//!   raw results to the file in `$RESULTS`. It finds the server in the `PG*` variables. The
//!   harness also gives it `RPG_SOCKET`, the socket directory of the server, and `RPG_ROOT`, the
//!   root of this repository.
//! - `setup.sql`: statements that the harness runs as the superuser in the new database of the
//!   suite before each run. It is optional.
//! - `oracle-fail.txt`: the tests that fail on the oracle. They are outside the denominator.
//! - `expected-fail.txt`: the tests that pass on the oracle and fail on rudb.
//!
//! A line of a list is a test name, then optionally a tab and a note, for example the rudb issue.
//! A line that starts with `#` is a note. The harness checks out the tag into
//! `target/clients/<name>/<tag>` and checks that it is the pinned commit.
//!
//! The suite runs on the oracle first and then on the other server, each time in a new database
//! `client_<name>`. A test passes on the other server only when it passed on the oracle too, so
//! a test that did not run on the other server, for example after a panic, counts as failed. A
//! run differs when a test fails that is in no list, or when a test in a list passes. The twin
//! has no expected failures. `--accept` writes the lists that make the run pass, and a person
//! commits them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::client::{self, Client, Login};
use crate::json::{self, Json};
use crate::servers::{Kind, Server};
use crate::toml;

#[derive(Debug, Default)]
pub(crate) struct Options {
    pub(crate) accept: bool,
}

/// The pin and the format of one client.
struct Pin {
    repository: String,
    tag: String,
    commit: String,
    format: String,
    strip: String,
}

/// The names of the clients in `clients/`, in order.
pub(crate) fn names(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join("clients");
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("client.toml").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

/// Runs the suite of one client on both servers and returns the number of differences and the
/// result JSON.
pub(crate) fn run(
    root: &Path,
    name: &str,
    oracle: &Server,
    other: &Server,
    options: &Options,
) -> Result<(usize, Json), String> {
    let dir = root.join("clients").join(name);
    let pin = read_pin(&dir)?;
    let checkout = checkout(root, name, &pin)?;
    let setup = std::fs::read_to_string(dir.join("setup.sql")).ok();

    let a = drive(root, name, &pin, &checkout, setup.as_deref(), oracle)?;
    let b = drive(root, name, &pin, &checkout, setup.as_deref(), other)?;

    let oracle_fail = read_list(&dir.join("oracle-fail.txt"))?;
    let expected_fail = if other.kind == Kind::Rudb {
        read_list(&dir.join("expected-fail.txt"))?
    } else {
        BTreeMap::new()
    };

    let mut differences: Vec<(&str, String, &str)> = Vec::new();
    let failed_on_oracle: BTreeSet<&str> = failed(&a).collect();
    let passed_on_oracle: BTreeSet<&str> = passed(&a).collect();
    for test in &failed_on_oracle {
        if !oracle_fail.contains_key(*test) {
            differences.push((
                &oracle.name,
                (*test).to_string(),
                "fails and is not in oracle-fail.txt",
            ));
        }
    }
    for test in oracle_fail.keys() {
        if passed_on_oracle.contains(test.as_str()) {
            differences.push((&oracle.name, test.clone(), "passes and is in oracle-fail.txt"));
        } else if !failed_on_oracle.contains(test.as_str()) {
            differences.push((&oracle.name, test.clone(), "did not run and is in oracle-fail.txt"));
        }
    }
    let passed_on_other: BTreeSet<&str> = passed(&b).collect();
    let failed_on_other: Vec<&str> =
        passed_on_oracle.iter().filter(|t| !passed_on_other.contains(*t)).copied().collect();
    for test in &failed_on_other {
        if !expected_fail.contains_key(*test) {
            differences.push((
                &other.name,
                (*test).to_string(),
                "fails and is not in expected-fail.txt",
            ));
        }
    }
    for test in expected_fail.keys() {
        if passed_on_other.contains(test.as_str()) {
            differences.push((&other.name, test.clone(), "passes and is in expected-fail.txt"));
        }
    }

    println!(
        "{name:<14} {} {}/{} passed   {} {}/{} passed   {} differ",
        oracle.name,
        passed_on_oracle.len(),
        a.len(),
        other.name,
        passed_on_oracle.len() - failed_on_other.len(),
        passed_on_oracle.len(),
        differences.len()
    );
    for (server, test, what) in differences.iter().take(40) {
        println!("  {server}: {test}: {what}");
    }
    if differences.len() > 40 {
        println!("  and {} more in run/results/client-{name}.json", differences.len() - 40);
    }

    if options.accept {
        write_list(&dir.join("oracle-fail.txt"), &failed_on_oracle, &oracle_fail)?;
        if other.kind == Kind::Rudb {
            let failed: BTreeSet<&str> = failed_on_other.iter().copied().collect();
            write_list(&dir.join("expected-fail.txt"), &failed, &expected_fail)?;
        }
    }

    let names = |tests: &mut dyn Iterator<Item = &str>| Json::Array(tests.map(Json::str).collect());
    let json = Json::object(vec![
        ("client", Json::str(name)),
        ("tag", Json::str(&pin.tag)),
        ("commit", Json::str(&pin.commit)),
        ("oracle", Json::str(&oracle.name)),
        ("other", Json::str(&other.name)),
        ("oracle_fail", Json::Number(oracle_fail.len() as i64)),
        ("expected_fail", Json::Number(expected_fail.len() as i64)),
        (
            "servers",
            Json::Array(vec![
                Json::object(vec![
                    ("name", Json::str(&oracle.name)),
                    ("passed", Json::Number(passed_on_oracle.len() as i64)),
                    ("total", Json::Number(a.len() as i64)),
                    ("failed", names(&mut failed_on_oracle.iter().copied())),
                ]),
                Json::object(vec![
                    ("name", Json::str(&other.name)),
                    (
                        "passed",
                        Json::Number((passed_on_oracle.len() - failed_on_other.len()) as i64),
                    ),
                    ("total", Json::Number(passed_on_oracle.len() as i64)),
                    ("failed", names(&mut failed_on_other.iter().copied())),
                ]),
            ]),
        ),
        (
            "differences",
            Json::Array(
                differences
                    .iter()
                    .map(|(server, test, what)| {
                        Json::object(vec![
                            ("server", Json::str(server)),
                            ("test", Json::str(test)),
                            ("what", Json::str(what)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ]);
    Ok((differences.len(), json))
}

fn passed(results: &BTreeMap<String, bool>) -> impl Iterator<Item = &str> {
    results.iter().filter(|(_, pass)| **pass).map(|(t, _)| t.as_str())
}

fn failed(results: &BTreeMap<String, bool>) -> impl Iterator<Item = &str> {
    results.iter().filter(|(_, pass)| !**pass).map(|(t, _)| t.as_str())
}

fn read_pin(dir: &Path) -> Result<Pin, String> {
    let path = dir.join("client.toml");
    let file = path.display().to_string();
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {file}: {e}"))?;
    let sections = toml::parse(&file, &text)?;
    let section = sections
        .iter()
        .find(|s| s.name == "client")
        .ok_or_else(|| format!("{file} has no [client] section"))?;
    let pin = Pin {
        repository: section.require(&file, "repository")?.to_string(),
        tag: section.require(&file, "tag")?.to_string(),
        commit: section.require(&file, "commit")?.to_string(),
        format: section.require(&file, "format")?.to_string(),
        strip: section.get("strip").unwrap_or_default().to_string(),
    };
    if !["go-json", "junit"].contains(&pin.format.as_str()) {
        return Err(format!("{file}: the format {:?} is not go-json or junit", pin.format));
    }
    Ok(pin)
}

/// Clones the tag once, and checks that it is the pinned commit each time.
fn checkout(root: &Path, name: &str, pin: &Pin) -> Result<PathBuf, String> {
    let dir = root.join("target").join("clients").join(name).join(&pin.tag);
    if !dir.exists() {
        std::fs::create_dir_all(dir.parent().unwrap_or(root))
            .map_err(|e| format!("cannot create the directory of {}: {e}", dir.display()))?;
        let status = Command::new("git")
            .args(["clone", "--quiet", "--depth", "1", "--branch", &pin.tag, &pin.repository])
            .arg(&dir)
            .status()
            .map_err(|e| format!("cannot start git: {e}"))?;
        if !status.success() {
            return Err(format!("cannot clone {} at {}", pin.repository, pin.tag));
        }
    }
    let head = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("cannot start git: {e}"))?;
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if head != pin.commit {
        return Err(format!(
            "{} is at {head}, and the pin of {name} is {} at {}",
            dir.display(),
            pin.tag,
            pin.commit
        ));
    }
    Ok(dir)
}

/// Makes the database of the suite new, runs the setup and the suite, and reads the results.
fn drive(
    root: &Path,
    name: &str,
    pin: &Pin,
    checkout: &Path,
    setup: Option<&str>,
    server: &Server,
) -> Result<BTreeMap<String, bool>, String> {
    let database = format!("client_{name}");
    fresh(server, &database, setup)?;

    let out = root.join("run").join("clients").join(name);
    std::fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let results = out.join(format!("{}.results", server.name));
    let _ = std::fs::remove_file(&results);
    let log = out.join(format!("{}.log", server.name));
    let log_file =
        std::fs::File::create(&log).map_err(|e| format!("cannot create {}: {e}", log.display()))?;
    let err_file = log_file.try_clone().map_err(|e| e.to_string())?;

    let mut command = Command::new("bash");
    command.arg(root.join("clients").join(name).join("run.sh")).current_dir(checkout);
    // Any other value from the shell of the person who runs the harness would change the run.
    for (key, _) in std::env::vars() {
        if key.starts_with("PG") {
            command.env_remove(key);
        }
    }
    command
        .env("PGHOST", "127.0.0.1")
        .env("PGPORT", server.port.to_string())
        .env("PGUSER", "postgres")
        .env("PGPASSWORD", "postgres")
        .env("PGDATABASE", &database)
        .env("PGSSLMODE", "disable")
        .env("RPG_SOCKET", &server.socket)
        .env("RPG_ROOT", root)
        .env("RESULTS", &results)
        .stdout(log_file)
        .stderr(err_file);
    println!("{name}: running the suite on {}, the log is {}", server.name, log.display());
    command.status().map_err(|e| format!("cannot start run.sh of {name}: {e}"))?;

    let text = std::fs::read_to_string(&results).map_err(|e| {
        format!("run.sh of {name} wrote no results on {}: {e}, see {}", server.name, log.display())
    })?;
    let tests = match pin.format.as_str() {
        "go-json" => go_json(&text, &pin.strip),
        _ => junit(&text, &pin.strip),
    };
    if tests.is_empty() {
        return Err(format!("{name} ran no test on {}, see {}", server.name, log.display()));
    }
    Ok(tests)
}

/// Drops and makes the database of the suite, and runs the setup in it.
fn fresh(server: &Server, database: &str, setup: Option<&str>) -> Result<(), String> {
    let mut admin =
        Client::connect(server, &Login::superuser("postgres")).map_err(|r| r.message)?;
    for sql in [
        format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"),
        format!("CREATE DATABASE {database} TEMPLATE template0"),
    ] {
        check(server, &sql, admin.simple(&sql))?;
    }
    if let Some(setup) = setup {
        let mut admin =
            Client::connect(server, &Login::superuser(database)).map_err(|r| r.message)?;
        check(server, "setup.sql", admin.simple(setup))?;
    }
    Ok(())
}

fn check(
    server: &Server,
    what: &str,
    frames: std::io::Result<Vec<crate::frame::Frame>>,
) -> Result<(), String> {
    let frames = frames.map_err(|e| format!("{}: {e}", server.name))?;
    match frames.iter().find(|f| f.tag == b'E') {
        Some(error) => Err(format!("{}: {what}: {}", server.name, client::error_text(error))),
        None => Ok(()),
    }
}

/// The result of each test in the output of `go test -json`. A package that fails without a
/// failed test, for example when it does not build, is one failed test named after the package.
fn go_json(text: &str, strip: &str) -> BTreeMap<String, bool> {
    let mut tests = BTreeMap::new();
    let mut failed_packages = Vec::new();
    for line in text.lines() {
        let Ok(event) = json::parse(line) else { continue };
        let field = |key| event.get(key).and_then(Json::as_str);
        let pass = match field("Action") {
            Some("pass") => true,
            Some("fail") => false,
            _ => continue,
        };
        let package = field("Package").unwrap_or_default();
        let package = package.strip_prefix(strip).unwrap_or(package);
        match field("Test") {
            Some(test) => {
                tests.insert(format!("{package} {test}"), pass);
            }
            None if !pass => failed_packages.push(package.to_string()),
            None => {}
        }
    }
    for package in failed_packages {
        if !tests.keys().any(|t| t.split(' ').next() == Some(package.as_str())) {
            tests.insert(format!("{package} (package)"), false);
        }
    }
    tests
}

/// The result of each `testcase` of a JUnit XML file. A skipped test is not a result.
fn junit(text: &str, strip: &str) -> BTreeMap<String, bool> {
    let mut tests = BTreeMap::new();
    let mut rest = text;
    while let Some(start) = rest.find("<testcase") {
        rest = &rest[start + "<testcase".len()..];
        let Some(head_end) = rest.find('>') else { break };
        let head = &rest[..head_end];
        let (body, next) = if head.ends_with('/') {
            ("", &rest[head_end + 1..])
        } else {
            match rest.find("</testcase>") {
                Some(end) => (&rest[head_end + 1..end], &rest[end..]),
                None => (&rest[head_end + 1..], ""),
            }
        };
        rest = next;
        if body.contains("<skipped") {
            continue;
        }
        let class = attribute(head, "classname").unwrap_or_default();
        let class = class.strip_prefix(strip).unwrap_or(&class).to_string();
        let name = attribute(head, "name").unwrap_or_default();
        let pass = !body.contains("<failure") && !body.contains("<error");
        let id = if class.is_empty() { name } else { format!("{class}::{name}") };
        // A test that fails in its teardown has a second testcase with the same name. It fails.
        let entry = tests.entry(id).or_insert(true);
        *entry &= pass;
    }
    tests
}

fn attribute(head: &str, name: &str) -> Option<String> {
    let start = head.find(&format!(" {name}=\""))? + name.len() + 3;
    let end = head[start..].find('"')? + start;
    Some(
        head[start..end]
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// A list: each test with its note.
fn read_list(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    Ok(text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.split_once('\t') {
            Some((test, note)) => (test.to_string(), note.to_string()),
            None => (l.to_string(), String::new()),
        })
        .collect())
}

/// Writes a list with the notes of the old list, and keeps its first lines that are notes.
fn write_list(
    path: &Path,
    tests: &BTreeSet<&str>,
    old: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut out: String = std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .take_while(|l| l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    for test in tests {
        match old.get(*test).filter(|n| !n.is_empty()) {
            Some(note) => out.push_str(&format!("{test}\t{note}\n")),
            None => out.push_str(&format!("{test}\n")),
        }
    }
    std::fs::write(path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{go_json, junit};

    #[test]
    fn go_test_events_give_each_test() {
        let text = r#"{"Action":"run","Package":"github.com/jackc/pgx/v5/pgconn","Test":"TestA"}
{"Action":"pass","Package":"github.com/jackc/pgx/v5/pgconn","Test":"TestA","Elapsed":0.01}
{"Action":"fail","Package":"github.com/jackc/pgx/v5/pgconn","Test":"TestB/sub","Elapsed":0}
{"Action":"fail","Package":"github.com/jackc/pgx/v5/pgconn","Elapsed":1.5}
{"Action":"fail","Package":"github.com/jackc/pgx/v5/broken","Elapsed":0}
not json
"#;
        let tests = go_json(text, "github.com/jackc/pgx/v5/");
        let tests: Vec<(&str, bool)> = tests.iter().map(|(t, p)| (t.as_str(), *p)).collect();
        assert_eq!(
            tests,
            [("broken (package)", false), ("pgconn TestA", true), ("pgconn TestB/sub", false)]
        );
    }

    #[test]
    fn junit_test_cases_give_each_test_without_the_skipped_ones() {
        let text = r#"<testsuite><testcase classname="tests.test_a" name="test_x[&quot;1&quot;]" time="0.1"/>
<testcase classname="tests.test_a" name="test_y"><failure message="no">trace</failure></testcase>
<testcase classname="tests.test_a" name="test_z"><skipped message="no server"/></testcase>
</testsuite>"#;
        let tests = junit(text, "tests.");
        let tests: Vec<(&str, bool)> = tests.iter().map(|(t, p)| (t.as_str(), *p)).collect();
        assert_eq!(tests, [("test_a::test_x[\"1\"]", true), ("test_a::test_y", false)]);
    }
}
