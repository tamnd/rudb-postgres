//! Running other programs: git, the build script, initdb, pg_ctl and psql.

use std::process::{Command, Stdio};

/// Runs a command to the end and fails with its output when it fails. The output goes to the
/// error and not to the terminal, so that a command that works stays quiet.
pub(crate) fn run(command: &mut Command) -> Result<String, String> {
    let shown = show(command);
    let output =
        command.stdin(Stdio::null()).output().map_err(|e| format!("cannot start {shown}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!("{shown} failed with {}\n{stdout}{stderr}", output.status))
}

/// Runs a command with the terminal as its output, for the long ones where a person wants to see
/// progress.
pub(crate) fn run_loud(command: &mut Command) -> Result<(), String> {
    let shown = show(command);
    let status = command.status().map_err(|e| format!("cannot start {shown}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{shown} failed with {status}")) }
}

fn show(command: &Command) -> String {
    let mut shown = command.get_program().to_string_lossy().into_owned();
    for arg in command.get_args() {
        shown.push(' ');
        shown.push_str(&arg.to_string_lossy());
    }
    shown
}
