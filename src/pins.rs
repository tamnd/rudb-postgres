//! The commits in `pins.toml` that every number is taken against.

use std::path::{Path, PathBuf};

use crate::toml;

#[derive(Debug, Clone)]
pub(crate) struct Pins {
    pub(crate) postgres_repository: String,
    pub(crate) postgres: String,
    pub(crate) postgres_describe: String,
}

impl Pins {
    pub(crate) fn read(root: &Path) -> Result<Pins, String> {
        let path = root.join("pins.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let sections = toml::parse("pins.toml", &text)?;
        let section = |name: &str| {
            sections
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| format!("pins.toml has no [{name}] section"))
        };
        let postgres = section("postgres")?;
        // The [rudb] section is read by the report, which is not written yet.
        section("rudb")?;
        let pins = Pins {
            postgres_repository: postgres.require("pins.toml", "repository")?.to_string(),
            postgres: postgres.require("pins.toml", "commit")?.to_string(),
            postgres_describe: postgres.require("pins.toml", "describe")?.to_string(),
        };
        if pins.postgres.len() != 40 || !pins.postgres.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "pins.toml: the postgres commit {:?} is not a full hash",
                pins.postgres
            ));
        }
        Ok(pins)
    }

    /// The first 8 characters of the PostgreSQL commit, the way the notes write it.
    pub(crate) fn postgres_short(&self) -> &str {
        &self.postgres[..8]
    }
}

/// The repository root: the first directory at or above the current one that has `pins.toml`.
/// The commands write under it, so they work from any directory inside the checkout.
pub(crate) fn root() -> Result<PathBuf, String> {
    let start = std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?;
    let mut dir = start.as_path();
    loop {
        if dir.join("pins.toml").is_file() {
            return Ok(dir.to_path_buf());
        }
        dir = dir.parent().ok_or_else(|| {
            format!(
                "no pins.toml at or above {}, so this is not a rudb-postgres checkout",
                start.display()
            )
        })?;
    }
}
