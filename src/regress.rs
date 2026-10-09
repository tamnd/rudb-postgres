//! The runners of the core regression suite and the isolation specs.
//!
//! Both suites use the real driver programs from the pin: `pg_regress` and
//! `pg_isolation_regress`. The runner starts the driver once for each server, oracle first, in the
//! same mode as `make installcheck`. The driver makes the database `regression` from `template0`,
//! runs the schedule and compares each output with the expected files byte for byte. This is the
//! strict number of document 14 section 14.8 of the notes.
//!
//! The runner then compares the other server with a ratchet. For rudb, the ratchet is the
//! committed diffs in `corpus/<suite>/<test>.diffs`, one file for each test that fails. A diff
//! that is not the committed one is a difference, also when it is smaller. `--accept` writes the
//! new diffs, and a person commits them. For the twin, the ratchet is empty, because the twin must
//! pass every test that the oracle passes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::client::{Client, Login};
use crate::json::Json;
use crate::servers::{Kind, Server};

/// One of the two suites that a driver from the pin runs.
pub(crate) struct Suite {
    /// The name of the command and of the result file.
    pub(crate) name: &'static str,
    /// The driver program in `test/bin` of the oracle prefix.
    program: &'static str,
    /// The input directory in `test/` of the oracle prefix.
    dir: &'static str,
    /// The schedule file in the input directory.
    schedule: &'static str,
    /// Whether the driver needs `--dlpath` for `regress.so`.
    dlpath: bool,
    /// The test that makes the tables of the other tests. A run of some tests starts with it.
    setup: Option<&'static str>,
}

pub(crate) const REGRESS: Suite = Suite {
    name: "regress",
    program: "pg_regress",
    dir: "regress",
    schedule: "parallel_schedule",
    dlpath: true,
    setup: Some("test_setup"),
};

pub(crate) const ISOLATION: Suite = Suite {
    name: "isolation",
    program: "pg_isolation_regress",
    dir: "isolation",
    schedule: "isolation_schedule",
    dlpath: false,
    setup: None,
};

#[derive(Debug, Default)]
pub(crate) struct Options {
    /// Run these tests and not the schedule.
    pub(crate) tests: Vec<String>,
    /// Write the diffs of the other server into the corpus.
    pub(crate) accept: bool,
}

/// The result of one driver run on one server.
pub(crate) struct Run {
    /// Each test in the order of the run, and whether it passed.
    pub(crate) outcomes: Vec<(String, bool)>,
    /// The diff of each failed test, with the paths and the times removed.
    diffs: BTreeMap<String, String>,
}

impl Run {
    pub(crate) fn passed(&self) -> usize {
        self.outcomes.iter().filter(|(_, ok)| *ok).count()
    }

    fn failed(&self) -> Vec<&str> {
        self.outcomes.iter().filter(|(_, ok)| !ok).map(|(name, _)| name.as_str()).collect()
    }
}

/// Runs the suite on both servers and returns the number of differences and the result JSON.
pub(crate) fn run(
    root: &Path,
    prefix: &Path,
    suite: &Suite,
    oracle: &Server,
    other: &Server,
    options: &Options,
) -> Result<(usize, Json), String> {
    let oracle_run = drive(root, prefix, suite, oracle, &options.tests)?;
    let other_run = drive(root, prefix, suite, other, &options.tests)?;

    let ratchet_dir = root.join("corpus").join(suite.name);
    let ratchet =
        if other.kind == Kind::Rudb { read_ratchet(&ratchet_dir)? } else { BTreeMap::new() };

    let mut differences = Vec::new();
    for name in oracle_run.failed() {
        differences.push((name.to_string(), "fails on the oracle".to_string()));
    }
    let ran: Vec<&str> = other_run.outcomes.iter().map(|(name, _)| name.as_str()).collect();
    for name in &ran {
        let found = other_run.diffs.get(*name);
        let expected = ratchet.get(*name);
        let what = match (found, expected) {
            (None, None) => continue,
            (Some(a), Some(b)) if a == b => continue,
            (Some(_), None) => "fails and has no committed diff",
            (None, Some(_)) => "passes and still has a committed diff",
            (Some(_), Some(_)) => "fails with a diff that is not the committed one",
        };
        differences.push(((*name).to_string(), what.to_string()));
    }

    println!(
        "{:<10} {} {}/{} passed   {} {}/{} passed   {} differ",
        suite.name,
        oracle.name,
        oracle_run.passed(),
        oracle_run.outcomes.len(),
        other.name,
        other_run.passed(),
        other_run.outcomes.len(),
        differences.len()
    );
    for (name, what) in differences.iter().take(40) {
        println!("  {name}: {what}");
    }
    if differences.len() > 40 {
        println!("  and {} more", differences.len() - 40);
    }

    if options.accept {
        if other.kind != Kind::Rudb {
            return Err(
                "--accept writes the diffs of rudb, and the other server is not rudb".into()
            );
        }
        accept(&ratchet_dir, &ran, &other_run.diffs)?;
        println!(
            "wrote the diffs of {} failed tests to {}",
            other_run.diffs.len(),
            ratchet_dir.display()
        );
    }

    let json = Json::object(vec![
        ("suite", Json::str(suite.name)),
        ("servers", Json::Array(vec![summary(oracle, &oracle_run), summary(other, &other_run)])),
        (
            "differences",
            Json::Array(
                differences
                    .iter()
                    .map(|(name, what)| {
                        Json::object(vec![("test", Json::str(name)), ("what", Json::str(what))])
                    })
                    .collect(),
            ),
        ),
    ]);
    Ok((differences.len(), json))
}

fn summary(server: &Server, run: &Run) -> Json {
    Json::object(vec![
        ("name", Json::str(&server.name)),
        ("passed", Json::Number(run.passed() as i64)),
        ("total", Json::Number(run.outcomes.len() as i64)),
        ("failed", Json::Array(run.failed().into_iter().map(Json::str).collect())),
    ])
}

/// Runs the driver against one server and reads its output.
pub(crate) fn drive(
    root: &Path,
    prefix: &Path,
    suite: &Suite,
    server: &Server,
    tests: &[String],
) -> Result<Run, String> {
    let input = prefix.join("test").join(suite.dir);
    let out = root.join("run").join(suite.name).join(&server.name);
    if out.exists() {
        std::fs::remove_dir_all(&out)
            .map_err(|e| format!("cannot remove {}: {e}", out.display()))?;
    }
    std::fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;

    clean(server);

    let mut command = Command::new(prefix.join("test").join("bin").join(suite.program));
    command
        .arg(format!("--inputdir={}", input.display()))
        .arg(format!("--expecteddir={}", input.display()))
        .arg(format!("--outputdir={}", out.display()))
        .arg(format!("--bindir={}", prefix.join("bin").display()))
        .arg(format!("--host={}", server.socket.display()))
        .arg(format!("--port={}", server.port));
    if suite.dlpath {
        command.arg(format!("--dlpath={}", prefix.join("test").join("bin").display()));
    }
    if tests.is_empty() {
        command.arg(format!("--schedule={}", input.join(suite.schedule).display()));
    } else {
        if let Some(setup) = suite.setup.filter(|setup| !tests.iter().any(|t| t == setup)) {
            command.arg(setup);
        }
        command.args(tests);
    }
    // The driver sets the variables it needs. Any other value from the shell of the person who
    // runs the harness would change the run.
    for name in ["PGHOST", "PGHOSTADDR", "PGPORT", "PGDATABASE", "PGOPTIONS", "PGSSLMODE"] {
        command.env_remove(name);
    }
    command.env("PGUSER", "postgres");

    let output = command.output().map_err(|e| format!("cannot start {}: {e}", suite.program))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(out.join("output.txt"), &text)
        .map_err(|e| format!("cannot write the output of {}: {e}", suite.program))?;
    let outcomes = outcomes(&text);
    if outcomes.is_empty() {
        return Err(format!(
            "{} ran no test on {}, so the run has no result\n{text}",
            suite.program, server.name
        ));
    }
    // The diffs of the encoding tests have bytes that are not UTF-8, and a diff that does not
    // read is not a diff that is not there.
    let diffs = match std::fs::read(out.join("regression.diffs")) {
        Ok(bytes) => split_diffs(&String::from_utf8_lossy(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(format!("cannot read the diffs of {}: {e}", server.name)),
    };
    Ok(Run { outcomes, diffs })
}

/// Drops what an earlier run that stopped early can leave in the cluster. The driver drops the
/// database `regression` itself, but the tablespace of `test_setup` is not in a database, and only
/// the test `tablespace` near the end of the schedule drops it. This is best effort: a server that
/// refuses the statements fails the tests that need them, and the run shows that.
pub(crate) fn clean(server: &Server) {
    let Ok(mut admin) = Client::connect(server, &Login::superuser("postgres")) else { return };
    for sql in [
        "DROP DATABASE IF EXISTS regression WITH (FORCE)",
        "DROP TABLESPACE IF EXISTS regress_tblspace",
    ] {
        let _ = admin.simple(sql);
    }
}

/// Reads the TAP lines of the driver: `ok 2 + boolean 118 ms` or `not ok 7 - int4 90 ms`. A `+`
/// marks a test in a parallel group.
fn outcomes(text: &str) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    for line in text.lines() {
        let (ok, rest) = if let Some(rest) = line.strip_prefix("not ok ") {
            (false, rest)
        } else if let Some(rest) = line.strip_prefix("ok ") {
            (true, rest)
        } else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let number = words.next();
        let marker = words.next();
        let name = words.next();
        if let (Some(number), Some("-" | "+"), Some(name)) = (number, marker, name)
            && number.bytes().all(|b| b.is_ascii_digit())
        {
            found.push((name.to_string(), ok));
        }
    }
    found
}

/// Splits `regression.diffs` into one diff for each test.
///
/// Each part starts with `diff -U3 <expected> <results>` or `diff -c ...`. The paths and the
/// times in the header lines change from run to run, so the header is replaced with the relative
/// names. The test name is the name of the results file.
fn split_diffs(text: &str) -> BTreeMap<String, String> {
    let mut diffs = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if line.starts_with("diff ") {
            if let Some((name, body)) = current.take() {
                diffs.insert(name, body);
            }
            let results = line.split_whitespace().last().unwrap_or_default();
            let file = results.rsplit('/').next().unwrap_or(results);
            let name = file.strip_suffix(".out").unwrap_or(file).to_string();
            let expected = line.split_whitespace().rev().nth(1).unwrap_or_default();
            let expected = expected.rsplit('/').next().unwrap_or(expected);
            current = Some((name, format!("diff expected/{expected} results/{file}\n")));
            continue;
        }
        let Some((_, body)) = current.as_mut() else { continue };
        if line.starts_with("--- /") || line.starts_with("+++ /") || line.starts_with("*** /") {
            // The header lines of the two files, with absolute paths and times.
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    if let Some((name, body)) = current {
        diffs.insert(name, body);
    }
    diffs
}

fn read_ratchet(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut found = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(found) };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(test) = name.strip_suffix(".diffs") else { continue };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        found.insert(test.to_string(), text);
    }
    Ok(found)
}

/// Writes the diff of each failed test that ran, and removes the diff of each test that passed.
fn accept(dir: &Path, ran: &[&str], diffs: &BTreeMap<String, String>) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for name in ran {
        let path: PathBuf = dir.join(format!("{name}.diffs"));
        match diffs.get(*name) {
            Some(diff) => std::fs::write(&path, diff)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?,
            None if path.exists() => std::fs::remove_file(&path)
                .map_err(|e| format!("cannot remove {}: {e}", path.display()))?,
            None => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{outcomes, split_diffs};

    #[test]
    fn the_tap_lines_give_each_test_and_its_result() {
        let text = "\
# using postmaster on /tmp/sock, port 55432
ok 1         - test_setup                                472 ms
# parallel group (2 tests):  boolean char
ok 2         + boolean                                   118 ms
not ok 3     + char                                       70 ms
# (test process exited with exit code 2)
1..3
";
        assert_eq!(
            outcomes(text),
            [
                ("test_setup".to_string(), true),
                ("boolean".to_string(), true),
                ("char".to_string(), false)
            ]
        );
    }

    #[test]
    fn the_diffs_split_by_test_without_paths_or_times() {
        let text = "\
diff -U3 /p/test/regress/expected/char.out /r/run/regress/rudb/results/char.out
--- /p/test/regress/expected/char.out\t2026-10-05 16:00:00
+++ /r/run/regress/rudb/results/char.out\t2026-10-05 16:01:00
@@ -1,2 +1,2 @@
-a
+b
diff -U3 /p/test/regress/expected/int4.out /r/run/regress/rudb/results/int4.out
--- /p/test/regress/expected/int4.out\t2026-10-05 16:00:00
+++ /r/run/regress/rudb/results/int4.out\t2026-10-05 16:01:00
@@ -9 +9 @@
-1
+2
";
        let diffs = split_diffs(text);
        assert_eq!(diffs.len(), 2);
        assert_eq!(
            diffs["char"],
            "diff expected/char.out results/char.out\n@@ -1,2 +1,2 @@\n-a\n+b\n"
        );
        assert!(
            diffs["int4"].starts_with("diff expected/int4.out results/int4.out\n@@ -9 +9 @@\n")
        );
    }
}
