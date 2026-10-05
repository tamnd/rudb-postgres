//! The small part of TOML that `pins.toml` and `run/servers.toml` use.
//!
//! A file is a list of `[section]` headers, each followed by `key = "value"` lines. Comments and
//! blank lines are ignored. Values are strings. That is all the harness writes and all it reads,
//! so a full TOML parser would be a dependency for nothing.

use std::fmt::Write as _;

/// One `[section]` and its keys, in the order of the file.
#[derive(Debug, Clone, Default)]
pub(crate) struct Section {
    pub(crate) name: String,
    pub(crate) keys: Vec<(String, String)>,
}

impl Section {
    pub(crate) fn new(name: &str) -> Section {
        Section { name: name.to_string(), keys: Vec::new() }
    }

    pub(crate) fn set(&mut self, key: &str, value: impl Into<String>) -> &mut Section {
        self.keys.push((key.to_string(), value.into()));
        self
    }

    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.keys.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// The value of a key that must be there. The error names the file and the section, because
    /// the person who reads it has to open the file.
    pub(crate) fn require(&self, file: &str, key: &str) -> Result<&str, String> {
        self.get(key).ok_or_else(|| format!("{file}: [{}] has no {key}", self.name))
    }
}

pub(crate) fn parse(file: &str, text: &str) -> Result<Vec<Section>, String> {
    let mut sections: Vec<Section> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = || format!("{file}:{}", i + 1);
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push(Section::new(name.trim()));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("{}: expected key = \"value\", found {line:?}", at()));
        };
        let value = value.trim();
        let Some(value) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
            return Err(format!("{}: a value must be a quoted string, found {value}", at()));
        };
        let Some(section) = sections.last_mut() else {
            return Err(format!("{}: a key before the first [section]", at()));
        };
        section.keys.push((key.trim().to_string(), unescape(value)));
    }
    Ok(sections)
}

pub(crate) fn write(sections: &[Section]) -> String {
    let mut out = String::new();
    for (i, section) in sections.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = writeln!(out, "[{}]", section.name);
        for (key, value) in &section.keys {
            let _ = writeln!(out, "{key} = \"{}\"", escape(value));
        }
    }
    out
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn unescape(value: &str) -> String {
    value.replace("\\\"", "\"").replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_file_reads_back_the_same() {
        let mut section = Section::new("oracle");
        section.set("port", "55432").set("path", r#"/tmp/a "b"\c"#);
        let text = write(&[section, Section::new("twin")]);
        let back = parse("t", &text).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].get("path"), Some(r#"/tmp/a "b"\c"#));
        assert_eq!(back[1].name, "twin");
    }

    #[test]
    fn a_bare_value_is_an_error() {
        let error = parse("pins.toml", "[a]\nport = 5432\n").unwrap_err();
        assert_eq!(error, "pins.toml:2: a value must be a quoted string, found 5432");
    }
}
