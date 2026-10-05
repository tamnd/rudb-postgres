//! The oracle: PostgreSQL built from the pin, per document 14 section 14.3 of the notes.
//!
//! The build goes into `target/oracle/<pin>`. It is cached by the pin and by the build script,
//! because the script holds the flags. When either changes, the build runs again and every number
//! must be taken again.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::pins::Pins;
use crate::process;

/// The install prefix of the oracle for a pin.
pub(crate) fn prefix(root: &Path, pins: &Pins) -> PathBuf {
    root.join("target").join("oracle").join(&pins.postgres)
}

/// The prefix of a finished build, or an error that says how to make one.
pub(crate) fn installed(root: &Path, pins: &Pins) -> Result<PathBuf, String> {
    let prefix = prefix(root, pins);
    match std::fs::read_to_string(prefix.join("PIN")) {
        Ok(pin) if pin.trim() == pins.postgres => Ok(prefix),
        _ => Err(format!(
            "no oracle for {} in {}, run `rudb-postgres oracle build` first",
            pins.postgres_short(),
            prefix.display()
        )),
    }
}

pub(crate) fn build(root: &Path, pins: &Pins) -> Result<(), String> {
    let prefix = prefix(root, pins);
    let script = root.join("oracle").join("build.sh");
    let flags = std::fs::read(&script)
        .map(|bytes| format!("{:016x}", fnv1a(&bytes)))
        .map_err(|e| format!("cannot read {}: {e}", script.display()))?;
    let built = std::fs::read_to_string(prefix.join("FLAGS")).unwrap_or_default();
    if installed(root, pins).is_ok() && built.trim() == flags {
        println!(
            "oracle {} ({}) is up to date in {}",
            pins.postgres_short(),
            pins.postgres_describe,
            prefix.display()
        );
        return Ok(());
    }

    let source = source(root, pins)?;
    println!(
        "building oracle {} ({}) from {}",
        pins.postgres_short(),
        pins.postgres_describe,
        source.display()
    );
    process::run_loud(Command::new(&script).arg(&source).arg(&prefix))?;
    std::fs::write(prefix.join("FLAGS"), format!("{flags}\n"))
        .map_err(|e| format!("cannot write {}: {e}", prefix.join("FLAGS").display()))?;
    println!(
        "oracle {} ({}) installed in {}",
        pins.postgres_short(),
        pins.postgres_describe,
        prefix.display()
    );
    Ok(())
}

/// A PostgreSQL checkout at the pin. `RUDB_POSTGRES_SOURCE` names an existing checkout. Without
/// it, the commit is fetched into `target/oracle/src`, and only that commit, because the full
/// history is ten times the size of the tree.
fn source(root: &Path, pins: &Pins) -> Result<PathBuf, String> {
    let dir = match std::env::var_os("RUDB_POSTGRES_SOURCE") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let dir = root.join("target").join("oracle").join("src");
            if !dir.join(".git").exists() {
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
                process::run(Command::new("git").arg("-C").arg(&dir).args(["init", "-q"]))?;
            }
            let at = head(&dir).unwrap_or_default();
            if at != pins.postgres {
                println!("fetching {} from {}", pins.postgres_short(), pins.postgres_repository);
                process::run_loud(Command::new("git").arg("-C").arg(&dir).args([
                    "fetch",
                    "-q",
                    "--depth",
                    "1",
                    &pins.postgres_repository,
                    &pins.postgres,
                ]))?;
                process::run(Command::new("git").arg("-C").arg(&dir).args([
                    "checkout",
                    "-q",
                    "FETCH_HEAD",
                ]))?;
            }
            dir
        }
    };
    let at = head(&dir)?;
    if at != pins.postgres {
        return Err(format!("{} is at {at}, and the pin is {}", dir.display(), pins.postgres));
    }
    Ok(dir)
}

fn head(dir: &Path) -> Result<String, String> {
    process::run(Command::new("git").arg("-C").arg(dir).args(["rev-parse", "HEAD"]))
        .map(|s| s.trim().to_string())
}

/// FNV-1a, 64 bits. It names a build, so it needs to be stable and not secure.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    #[test]
    fn fnv1a_matches_the_reference_values() {
        assert_eq!(super::fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(super::fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
