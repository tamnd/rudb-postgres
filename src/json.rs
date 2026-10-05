//! JSON for the result files. The commands write it, and the report reads it back. The reader
//! takes the JSON that the writer makes and standard JSON in general, except that a number must
//! be an integer.

use std::fmt::Write as _;

#[derive(Debug, Clone)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(i64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn object(fields: Vec<(&str, Json)>) -> Json {
        Json::Object(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub(crate) fn str(s: &str) -> Json {
        Json::String(s.to_string())
    }

    /// The value with two spaces of indent, which makes a diff of two result files readable.
    pub(crate) fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, out: &mut String, indent: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Number(n) => {
                let _ = write!(out, "{n}");
            }
            Json::String(s) => quote(out, s),
            Json::Array(items) if items.is_empty() => out.push_str("[]"),
            Json::Object(fields) if fields.is_empty() => out.push_str("{}"),
            Json::Array(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&"  ".repeat(indent + 1));
                    item.write(out, indent + 1);
                    out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                }
                out.push_str(&"  ".repeat(indent));
                out.push(']');
            }
            Json::Object(fields) => {
                out.push_str("{\n");
                for (i, (key, value)) in fields.iter().enumerate() {
                    out.push_str(&"  ".repeat(indent + 1));
                    quote(out, key);
                    out.push_str(": ");
                    value.write(out, indent + 1);
                    out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
                }
                out.push_str(&"  ".repeat(indent));
                out.push('}');
            }
        }
    }
}

impl Json {
    /// The value of a field of an object. `None` for a missing field or a value that is not an
    /// object.
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn items(&self) -> &[Json] {
        match self {
            Json::Array(items) => items,
            _ => &[],
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// Reads one JSON value.
pub(crate) fn parse(text: &str) -> Result<Json, String> {
    let mut reader = Reader { bytes: text.as_bytes(), at: 0 };
    let value = reader.value()?;
    reader.space();
    if reader.at != reader.bytes.len() {
        return Err(reader.error("text after the value"));
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn error(&self, what: &str) -> String {
        format!("JSON at byte {}: {what}", self.at)
    }

    fn space(&mut self) {
        while self.bytes.get(self.at).is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        self.space();
        let found = self.bytes.get(self.at) == Some(&b);
        if found {
            self.at += 1;
        }
        found
    }

    fn word(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.error("an unknown word"))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.space();
        match self.bytes.get(self.at) {
            Some(b'n') => self.word("null", Json::Null),
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Ok(Json::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    if self.eat(b']') {
                        return Ok(Json::Array(items));
                    }
                    if !self.eat(b',') {
                        return Err(self.error("no comma or ] in an array"));
                    }
                }
            }
            Some(b'{') => {
                self.at += 1;
                let mut fields = Vec::new();
                if self.eat(b'}') {
                    return Ok(Json::Object(fields));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return Err(self.error("no colon after a key"));
                    }
                    fields.push((key, self.value()?));
                    if self.eat(b'}') {
                        return Ok(Json::Object(fields));
                    }
                    if !self.eat(b',') {
                        return Err(self.error("no comma or } in an object"));
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => {
                let start = self.at;
                self.at += 1;
                while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.bytes[start..self.at]).unwrap_or("");
                text.parse()
                    .map(Json::Number)
                    .map_err(|_| self.error("a number that is not an integer"))
            }
            _ => Err(self.error("no value")),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.bytes.get(self.at) != Some(&b'"') {
            return Err(self.error("no string"));
        }
        self.at += 1;
        let mut out = Vec::new();
        loop {
            let Some(&b) = self.bytes.get(self.at) else {
                return Err(self.error("a string without its end"));
            };
            self.at += 1;
            match b {
                b'"' => {
                    return String::from_utf8(out)
                        .map_err(|_| self.error("a string that is not UTF-8"));
                }
                b'\\' => {
                    let Some(&e) = self.bytes.get(self.at) else {
                        return Err(self.error("a string without its end"));
                    };
                    self.at += 1;
                    let c = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xd800..0xdc00).contains(&code)
                                && self.bytes[self.at..].starts_with(b"\\u")
                            {
                                self.at += 2;
                                let low = self.hex4()?;
                                code = 0x10000
                                    + ((code - 0xd800) << 10)
                                    + (low.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            char::from_u32(code).unwrap_or('\u{fffd}')
                        }
                        _ => return Err(self.error("an unknown escape")),
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(b),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let text = self.bytes.get(self.at..self.at + 4).and_then(|h| std::str::from_utf8(h).ok());
        let code = text.and_then(|h| u32::from_str_radix(h, 16).ok());
        self.at += 4;
        code.ok_or_else(|| self.error("a \\u escape without four hex digits"))
    }
}

fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::{Json, parse};

    #[test]
    fn the_reader_reads_what_the_writer_writes() {
        let value = Json::object(vec![
            ("a", Json::Array(vec![Json::Number(-1), Json::Null, Json::Bool(false)])),
            ("b", Json::str("x\"y\n\u{1}\\é")),
            ("c", Json::Object(Vec::new())),
        ]);
        let back = parse(&value.pretty()).unwrap();
        assert_eq!(back.pretty(), value.pretty());
        assert_eq!(back.get("b").and_then(Json::as_str), Some("x\"y\n\u{1}\\é"));
        assert_eq!(back.get("a").map(|a| a.items().len()), Some(3));
        assert_eq!(parse("\"\\ud83d\\ude00\"").unwrap().as_str(), Some("\u{1f600}"));
        assert!(parse("[1,]").is_err());
        assert!(parse("1.5").is_err());
    }

    #[test]
    fn strings_are_escaped_and_nesting_is_indented() {
        let value = Json::object(vec![
            ("a", Json::Array(vec![Json::Number(1), Json::Number(2)])),
            ("b", Json::str("x\"y\n\u{1}")),
            ("c", Json::Bool(true)),
        ]);
        assert_eq!(
            value.pretty(),
            "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": \"x\\\"y\\n\\u0001\",\n  \"c\": true\n}\n"
        );
    }
}
