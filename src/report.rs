//! The report page of document 14 section 14.10 of the notes.
//!
//! The report reads the result files in `run/results` and counts, for each denominator of
//! document 01 section 1.3, the cases that the other server passed. A result file counts only
//! when it names the other server of the page. A denominator with no such file is not run, and
//! each of its cases counts as failed.
//!
//! With `--rudb <commit>`, the page is the page of that rudb commit, and it goes to
//! `reports/<date>/<commit>.json` and `.md`. Without it, the page compares the oracle with the
//! other server of the last `up`, usually its twin, and goes to `run/report`. That page is a
//! check of the harness, and it is not published.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::oracle;
use crate::pins::Pins;
use crate::servers;

#[derive(Debug, Default)]
pub(crate) struct Options {
    /// The rudb commit of the page.
    pub(crate) rudb: Option<String>,
    /// The date of the page, `YYYY-MM-DD`. Today when it is not given.
    pub(crate) date: Option<String>,
}

/// What a denominator is out of when it did not run.
#[derive(Debug, Clone, Copy)]
enum Total {
    Count(usize),
    Plan,
    Text(&'static str),
}

/// The result of one denominator.
#[derive(Debug)]
struct Run {
    passed: usize,
    total: usize,
    /// The cases that failed, by a name that stays the same from page to page.
    failed: Vec<String>,
}

#[derive(Debug)]
struct Row {
    level: &'static str,
    name: &'static str,
    total: Total,
    run: Option<Run>,
}

/// Writes the page and returns the paths of the JSON and the Markdown.
pub(crate) fn run(
    root: &Path,
    pins: &Pins,
    options: &Options,
) -> Result<(PathBuf, PathBuf), String> {
    let other = match &options.rudb {
        Some(_) => "rudb".to_string(),
        None => servers::load(root)
            .ok()
            .and_then(|s| s.get(1).map(|s| s.name.clone()))
            .unwrap_or_else(|| "twin".to_string()),
    };
    let results = Results { dir: root.join("run").join("results"), other: other.clone() };
    let clients = results.clients()?;
    let rows = rows(&results, &clients)?;
    let date = options.date.clone().unwrap_or_else(today);
    let (dir, stem) = match &options.rudb {
        Some(commit) => {
            let short: String = commit.chars().take(8).collect();
            (root.join("reports").join(&date), short)
        }
        None => (root.join("run").join("report"), "report".to_string()),
    };
    let json_path = dir.join(format!("{stem}.json"));
    let previous = match &options.rudb {
        Some(_) => previous(&root.join("reports"), &json_path)?,
        None => None,
    };

    let pins_json = Json::object(vec![
        ("rudb", options.rudb.as_deref().map_or(Json::Null, Json::str)),
        ("other", Json::str(&other)),
        ("postgres", Json::str(&pins.postgres)),
        ("postgres_describe", Json::str(&pins.postgres_describe)),
        ("configuration", Json::str(&configuration(root)?)),
        ("machine", Json::str(&machine())),
        (
            "client_suites",
            Json::Array(
                clients
                    .iter()
                    .map(|c| {
                        let field = |key| c.get(key).cloned().unwrap_or(Json::Null);
                        Json::object(vec![
                            ("client", field("client")),
                            ("tag", field("tag")),
                            ("commit", field("commit")),
                        ])
                    })
                    .collect(),
            ),
        ),
    ]);
    let page = Json::object(vec![
        ("date", Json::str(&date)),
        ("pins", pins_json.clone()),
        ("rows", Json::Array(rows.iter().map(row_json).collect())),
        ("clients", Json::Array(clients.iter().map(client_json).collect())),
        ("logic", results.read("gen")?.and_then(|g| logic(&g, &other)).unwrap_or(Json::Null)),
    ]);
    let markdown = markdown(&date, &pins_json, &rows, &page, previous.as_ref());

    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let md_path = dir.join(format!("{stem}.md"));
    std::fs::write(&json_path, page.pretty())
        .map_err(|e| format!("cannot write {}: {e}", json_path.display()))?;
    std::fs::write(&md_path, markdown)
        .map_err(|e| format!("cannot write {}: {e}", md_path.display()))?;
    Ok((json_path, md_path))
}

struct Results {
    dir: PathBuf,
    other: String,
}

impl Results {
    /// Reads `run/results/<name>.json`, or `None` when the command has not run.
    fn read(&self, name: &str) -> Result<Option<Json>, String> {
        let path = self.dir.join(format!("{name}.json"));
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                json::parse(&text).map(Some).map_err(|e| format!("{}: {e}", path.display()))
            }
            Err(_) => Ok(None),
        }
    }

    /// Reads a result file that names the other server of the page in its `other` field.
    fn against(&self, name: &str) -> Result<Option<Json>, String> {
        Ok(self.read(name)?.filter(|j| j.get("other").and_then(Json::as_str) == Some(&self.other)))
    }

    /// The `client-<name>.json` files that name the other server, by client name.
    fn clients(&self) -> Result<Vec<Json>, String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Ok(Vec::new()) };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter_map(|n| n.strip_prefix("client-")?.strip_suffix(".json").map(str::to_string))
            .collect();
        names.sort();
        let mut clients = Vec::new();
        for name in names {
            clients.extend(self.against(&format!("client-{name}"))?);
        }
        Ok(clients)
    }
}

fn rows(results: &Results, clients: &[Json]) -> Result<Vec<Row>, String> {
    let row = |level, name, total, run| Row { level, name, total, run };
    Ok(vec![
        row("L1", "Protocol cases", Total::Plan, protocol(results)?),
        row("L1", "Connect matrix", Total::Count(14), None),
        row("L2", "Catalog corpus", Total::Plan, None),
        row(
            "L2",
            "Catalog shape",
            Total::Text("64 catalogs, 86 views, 65 `information_schema` views"),
            None,
        ),
        row("L3", "Regression suite, strict", Total::Count(239), suite(results, "regress")?),
        row("L3", "Regression suite, statements", Total::Plan, None),
        row("L3", "Differential corpus", Total::Plan, differential(results)?),
        row("L3", "Function coverage", Total::Count(3414), functions(results)?),
        row("L4", "Isolation specs", Total::Count(133), suite(results, "isolation")?),
        row("L4", "Hermitage cases", Total::Plan, None),
        row("L4", "Error corpus", Total::Count(7112), None),
        row("L4", "Settings", Total::Count(438), None),
        row("L5", "Upstream suites", Total::Count(CLIENTS), upstream(clients)),
    ])
}

/// The clients of the first matrix in document 14 section 14.6.
const CLIENTS: usize = 14;

/// The tests of one client that passed on the oracle and on the other server.
fn client_counts(client: &Json) -> (usize, usize) {
    let servers = client.get("servers").map_or(&[][..], Json::items);
    let passed = |i: usize| {
        servers.get(i).and_then(|s| s.get("passed")).and_then(Json::as_i64).unwrap_or(0) as usize
    };
    (passed(0), passed(1))
}

/// A client passes when its L5 number is 100 percent: each test that passed on the oracle
/// passed on the other server. A client of the matrix without a suite counts as failed.
fn upstream(clients: &[Json]) -> Option<Run> {
    if clients.is_empty() {
        return None;
    }
    let mut failed = Vec::new();
    for client in clients {
        let (oracle, other) = client_counts(client);
        if oracle == 0 || other < oracle {
            let name = client.get("client").and_then(Json::as_str).unwrap_or("?");
            failed.push(format!("client/{name}"));
        }
    }
    Some(Run { passed: clients.len() - failed.len(), total: CLIENTS, failed })
}

fn client_json(client: &Json) -> Json {
    let (oracle, other) = client_counts(client);
    let field = |key| client.get(key).cloned().unwrap_or(Json::Null);
    let total = client
        .get("servers")
        .and_then(|s| s.items().first())
        .and_then(|s| s.get("total"))
        .cloned()
        .unwrap_or(Json::Null);
    Json::object(vec![
        ("client", field("client")),
        ("tag", field("tag")),
        ("oracle_passed", Json::Number(oracle as i64)),
        ("oracle_total", total),
        ("passed", Json::Number(other as i64)),
        ("oracle_fail", field("oracle_fail")),
        ("expected_fail", field("expected_fail")),
    ])
}

/// The L5 number of a client, the passes on the other server over the passes on the oracle.
fn l5(oracle: i64, other: i64) -> String {
    if oracle == 0 {
        return "no test passed on the oracle".to_string();
    }
    if other == oracle {
        return "100 percent".to_string();
    }
    // Round down, so that a client below 100 percent never shows 100.0.
    let tenths = other * 1000 / oracle;
    format!("{}.{} percent", tenths / 10, tenths % 10)
}

/// The connect matrix of the harness and the replayed traces.
fn protocol(results: &Results) -> Result<Option<Run>, String> {
    let mut cases: Vec<(String, bool)> = Vec::new();
    if let Some(connect) = results.against("connect")? {
        for case in connect.get("cases").map_or(&[][..], Json::items) {
            let name = case.get("case").and_then(Json::as_str).unwrap_or("?");
            cases.push((
                format!("connect/{name}"),
                case.get("same").and_then(Json::as_bool) == Some(true),
            ));
        }
    }
    if let Some(replay) = results.against("replay")? {
        for trace in replay.get("traces").map_or(&[][..], Json::items) {
            let name = trace.get("trace").and_then(Json::as_str).unwrap_or("?");
            let same = trace.get("difference").and_then(Json::as_bool) == Some(false);
            cases.push((format!("replay/{name}"), same));
        }
    }
    Ok(count(cases))
}

/// `regress` and `isolation`, where the driver gives each server its own pass or fail.
fn suite(results: &Results, name: &str) -> Result<Option<Run>, String> {
    let Some(file) = results.read(name)? else { return Ok(None) };
    let Some(server) = file
        .get("servers")
        .map_or(&[][..], Json::items)
        .iter()
        .skip(1)
        .find(|s| s.get("name").and_then(Json::as_str) == Some(&results.other))
    else {
        return Ok(None);
    };
    let number = |key| server.get(key).and_then(Json::as_i64).unwrap_or(0) as usize;
    let failed = server
        .get("failed")
        .map_or(&[][..], Json::items)
        .iter()
        .filter_map(Json::as_str)
        .map(|t| format!("{name}/{t}"))
        .collect();
    Ok(Some(Run { passed: number("passed"), total: number("total"), failed }))
}

/// Each statement of the differential files and of the generated file. A statement passes when
/// it gives the same answer in each mode.
fn differential(results: &Results) -> Result<Option<Run>, String> {
    let mut files: Vec<Json> = Vec::new();
    if let Some(diff) = results.read("diff")? {
        files.extend(
            diff.items()
                .iter()
                .filter(|f| f.get("other").and_then(Json::as_str) == Some(&results.other))
                .cloned(),
        );
    }
    if let Some(generated) = results.against("gen")? {
        files.extend(generated.get("diff").cloned());
    }
    if files.is_empty() {
        return Ok(None);
    }
    let mut cases = Vec::new();
    for file in &files {
        let path = file.get("file").and_then(Json::as_str).unwrap_or("?");
        let name = Path::new(path).file_name().map_or(path.into(), |n| n.to_string_lossy());
        let mut lines: Vec<(i64, bool)> = Vec::new();
        for r in file.get("results").map_or(&[][..], Json::items) {
            let line = r.get("line").and_then(Json::as_i64).unwrap_or(0);
            let same = r.get("outcome").and_then(Json::as_str) == Some("same");
            match lines.iter_mut().find(|(l, _)| *l == line) {
                Some((_, all)) => *all &= same,
                None => lines.push((line, same)),
            }
        }
        cases.extend(lines.into_iter().map(|(line, same)| (format!("{name}:{line}"), same)));
    }
    Ok(count(cases))
}

/// The rows of `pg_proc.dat` whose generated calls all gave the same answer. A function that
/// the generator did not call does not pass.
fn functions(results: &Results) -> Result<Option<Run>, String> {
    let Some(generated) = results.against("gen")? else { return Ok(None) };
    let total = generated.get("catalog_functions").and_then(Json::as_i64).unwrap_or(0) as usize;
    let mut passed = 0;
    let mut failed = Vec::new();
    for f in generated.get("functions").map_or(&[][..], Json::items) {
        let oid = f.get("oid").and_then(Json::as_i64).unwrap_or(0);
        let calls = f.get("calls").and_then(Json::as_i64).unwrap_or(0);
        let differ = f.get("differ").and_then(Json::as_i64).unwrap_or(0);
        if oid >= 10000 || calls == 0 {
            continue;
        }
        if differ == 0 {
            passed += 1;
        } else {
            let name = f.get("name").and_then(Json::as_str).unwrap_or("?");
            let args = f.get("args").and_then(Json::as_str).unwrap_or("");
            failed.push(format!("function/{oid} {name}({})", args.replace(' ', ", ")));
        }
    }
    Ok(Some(Run { passed, total, failed }))
}

fn count(cases: Vec<(String, bool)>) -> Option<Run> {
    if cases.is_empty() {
        return None;
    }
    let total = cases.len();
    let failed: Vec<String> =
        cases.into_iter().filter(|(_, same)| !same).map(|(name, _)| name).collect();
    Some(Run { passed: total - failed.len(), total, failed })
}

fn logic(generated: &Json, other: &str) -> Option<Json> {
    if generated.get("other").and_then(Json::as_str) != Some(other) {
        return None;
    }
    let seed = generated.get("seed").cloned().unwrap_or(Json::Null);
    let server = generated
        .get("logic")?
        .items()
        .iter()
        .find(|l| l.get("server").and_then(Json::as_str) == Some(other))?
        .clone();
    Some(Json::object(vec![
        ("seed", seed),
        ("checks", server.get("checks").cloned().unwrap_or(Json::Number(0))),
        ("failures", Json::Number(server.get("failures").map_or(0, |f| f.items().len()) as i64)),
    ]))
}

fn row_json(row: &Row) -> Json {
    let total = match (&row.run, row.total) {
        (Some(run), _) => Json::Number(run.total as i64),
        (None, Total::Count(n)) => Json::Number(n as i64),
        (None, Total::Plan) => Json::str("plan"),
        (None, Total::Text(text)) => Json::str(text),
    };
    Json::object(vec![
        ("level", Json::str(row.level)),
        ("denominator", Json::str(row.name)),
        ("run", Json::Bool(row.run.is_some())),
        ("passed", Json::Number(row.run.as_ref().map_or(0, |r| r.passed) as i64)),
        ("total", total),
        (
            "failed",
            Json::Array(
                row.run
                    .as_ref()
                    .map_or(&[][..], |r| &r.failed)
                    .iter()
                    .map(|f| Json::str(f))
                    .collect(),
            ),
        ),
    ])
}

fn markdown(date: &str, pins: &Json, rows: &[Row], page: &Json, previous: Option<&Json>) -> String {
    let get = |key| pins.get(key).and_then(Json::as_str).unwrap_or("");
    let mut out = String::new();
    let other = get("other");
    match pins.get("rudb").and_then(Json::as_str) {
        Some(commit) => {
            let short: String = commit.chars().take(8).collect();
            let _ = writeln!(out, "# PostgreSQL compatibility of rudb at {short}, {date}\n");
            if rows.iter().all(|r| r.run.is_none()) {
                out.push_str("No result file in `run/results` names rudb as the other server, so no denominator ran and each case counts as failed.\n\n");
            }
        }
        None => {
            let _ = writeln!(out, "# The oracle against {other}, {date}\n");
            out.push_str(
                "This page checks the harness. It is not a rudb page and it is not published.\n\n",
            );
        }
    }

    out.push_str("## Pins\n\n| Pin | Value |\n| --- | --- |\n");
    let _ = writeln!(
        out,
        "| rudb | {} |",
        pins.get("rudb")
            .and_then(Json::as_str)
            .unwrap_or("none, the other server is a second copy of the oracle")
    );
    let _ = writeln!(out, "| PostgreSQL | {} at {} |", get("postgres_describe"), get("postgres"));
    let suites: Vec<String> = pins
        .get("client_suites")
        .map_or(&[][..], Json::items)
        .iter()
        .map(|c| {
            let field = |key| c.get(key).and_then(Json::as_str).unwrap_or("?");
            let short: String = field("commit").chars().take(8).collect();
            format!("{} {} at `{short}`", field("client"), field("tag"))
        })
        .collect();
    let suites = if suites.is_empty() { "none ran".to_string() } else { suites.join(", ") };
    let _ = writeln!(out, "| Client suites | {suites} |");
    let _ = writeln!(out, "| Configuration | `{}` |", get("configuration"));
    let _ = writeln!(out, "| Machine | {} |\n", get("machine"));

    out.push_str("## Denominators\n\n");
    out.push_str("| Level | Denominator | Passed | Total | Change | Not counted |\n");
    out.push_str("| --- | --- | ---: | --- | --- | ---: |\n");
    let before = |name: &str| {
        previous?
            .get("rows")?
            .items()
            .iter()
            .find(|r| r.get("denominator").and_then(Json::as_str) == Some(name))
            .cloned()
    };
    for row in rows {
        let passed = row.run.as_ref().map_or(0, |r| r.passed);
        let total = match (&row.run, row.total) {
            (Some(run), _) => run.total.to_string(),
            (None, Total::Count(n)) => n.to_string(),
            (None, Total::Plan) => "plan".to_string(),
            (None, Total::Text(text)) => text.to_string(),
        };
        let change = match before(row.name).and_then(|r| r.get("passed").and_then(Json::as_i64)) {
            Some(was) if passed as i64 > was => format!("+{}", passed as i64 - was),
            Some(was) => format!("{}", passed as i64 - was),
            None => "first page".to_string(),
        };
        let total = if row.run.is_none() { format!("{total}, not run") } else { total };
        let _ =
            writeln!(out, "| {} | {} | {passed} | {total} | {change} | 0 |", row.level, row.name);
    }
    out.push_str("\nA denominator that did not run counts each of its cases as failed. ");
    out.push_str("No case is removed by the items of document 01 section 1.6 yet, so each row has 0 cases not counted.\n\n");

    out.push_str("## Clients\n\n");
    let clients = page.get("clients").map_or(&[][..], Json::items);
    if clients.is_empty() {
        out.push_str(
            "No client suite ran against this server, so this page has no client rows.\n\n",
        );
    } else {
        let _ = writeln!(
            out,
            "| Client | Tag | Oracle | {other} | L5 | Through PgBouncer | oracle-fail.txt | expected-fail.txt |"
        );
        out.push_str("| --- | --- | ---: | ---: | --- | --- | ---: | ---: |\n");
        for client in clients {
            let text = |key| client.get(key).and_then(Json::as_str).unwrap_or("?");
            let n = |key| client.get(key).and_then(Json::as_i64).unwrap_or(0);
            let _ = writeln!(
                out,
                "| {} | {} | {} of {} | {} | {} | not run | {} | {} |",
                text("client"),
                text("tag"),
                n("oracle_passed"),
                n("oracle_total"),
                n("passed"),
                l5(n("oracle_passed"), n("passed")),
                n("oracle_fail"),
                n("expected_fail")
            );
        }
        let _ = writeln!(
            out,
            "\nThe L5 number of a client is its tests that passed on {other} over its tests that passed on the oracle. A client is drop-in when this number is 100 percent and the two other conditions of document 01 section 1.4 hold. The harness does not check those two conditions yet. {} of the {CLIENTS} clients of the first matrix have no suite in `clients` yet.\n",
            CLIENTS.saturating_sub(clients.len())
        );
    }
    out.push_str("## Resources\n\nThe harness does not measure the server processes yet.\n\n");

    match page.get("logic") {
        Some(Json::Object(_)) => {
            let logic = page.get("logic").unwrap_or(&Json::Null);
            let n = |key| logic.get(key).and_then(Json::as_i64).unwrap_or(0);
            let seed = logic.get("seed").and_then(Json::as_str).unwrap_or("?");
            let _ = writeln!(
                out,
                "## Logic checks\n\nSeed {seed}: {} checks on {other}, {} failed.\n",
                n("checks"),
                n("failures")
            );
        }
        _ => out.push_str("## Logic checks\n\nThe generator did not run against this server.\n\n"),
    }

    out.push_str("## Changes\n\n");
    match previous {
        None => out.push_str("This is the first page, so no case changed state.\n"),
        Some(previous) => {
            let mut any = false;
            for row in rows {
                let Some(run) = &row.run else { continue };
                let Some(was) = before(row.name) else { continue };
                if was.get("run").and_then(Json::as_bool) != Some(true) {
                    continue;
                }
                let old: BTreeSet<&str> = was
                    .get("failed")
                    .map_or(&[][..], Json::items)
                    .iter()
                    .filter_map(Json::as_str)
                    .collect();
                let new: BTreeSet<&str> = run.failed.iter().map(String::as_str).collect();
                for case in new.difference(&old) {
                    let _ = writeln!(out, "- {}: new failure: {case}", row.name);
                    any = true;
                }
                for case in old.difference(&new) {
                    let _ = writeln!(out, "- {}: new pass: {case}", row.name);
                    any = true;
                }
            }
            if !any {
                let date = previous.get("date").and_then(Json::as_str).unwrap_or("?");
                let _ = writeln!(out, "No case changed state since the page of {date}.");
            }
        }
    }
    out
}

/// The latest page in `reports` other than this one.
fn previous(reports: &Path, this: &Path) -> Result<Option<Json>, String> {
    let mut pages: Vec<PathBuf> = Vec::new();
    let Ok(dates) = std::fs::read_dir(reports) else { return Ok(None) };
    for date in dates.flatten() {
        let Ok(files) = std::fs::read_dir(date.path()) else { continue };
        pages.extend(
            files
                .flatten()
                .map(|f| f.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json") && p != this),
        );
    }
    pages.sort_by_key(|p| {
        let modified = std::fs::metadata(p).and_then(|m| m.modified()).ok();
        (p.parent().map(Path::to_path_buf), modified)
    });
    match pages.last() {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            json::parse(&text).map(Some).map_err(|e| format!("{}: {e}", path.display()))
        }
        None => Ok(None),
    }
}

/// A hash of the files that configure the oracle, so that two pages with the same hash ran
/// under the same configuration.
fn configuration(root: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    for name in ["build.sh", "postgresql.conf", "pg_hba.conf", "setup.sql"] {
        let path = root.join("oracle").join(name);
        bytes.extend(
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?,
        );
    }
    Ok(format!("{:016x}", oracle::fnv1a(&bytes)))
}

fn machine() -> String {
    let uname = std::process::Command::new("uname")
        .arg("-sm")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
    format!("{uname}, {threads} threads")
}

/// Today in UTC, as `YYYY-MM-DD`.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = civil(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// The calendar date of a day number from 1970-01-01, by the algorithm of Howard Hinnant.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::{civil, count, l5};

    #[test]
    fn day_numbers_become_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
        assert_eq!(civil(20_731), (2026, 10, 5));
    }

    #[test]
    fn count_gives_the_failed_cases() {
        let run = count(vec![("a".into(), true), ("b".into(), false)]).unwrap();
        assert_eq!((run.passed, run.total), (1, 2));
        assert_eq!(run.failed, ["b"]);
        assert!(count(Vec::new()).is_none());
    }

    #[test]
    fn the_l5_number_never_rounds_up_to_100() {
        assert_eq!(l5(5089, 5089), "100 percent");
        assert_eq!(l5(5089, 5088), "99.9 percent");
        assert_eq!(l5(3, 2), "66.6 percent");
        assert_eq!(l5(0, 0), "no test passed on the oracle");
    }
}
