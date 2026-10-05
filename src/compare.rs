//! The comparison rules of document 01 section 1.5 of the notes.
//!
//! Two servers give the same answer when a client cannot tell them apart. The rules below say,
//! for each message type, which fields are compared. A field that a rule does not name is
//! compared exactly. Every exception here must be in that section of the notes, and a change to
//! a rule comes with a test that shows what the rule hid before.

use crate::frame::{self, Frame};

/// One difference between the oracle's frames and the other server's frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Difference {
    /// The index of the frame in the oracle's sequence.
    pub(crate) at: usize,
    /// What differs, in words.
    pub(crate) what: String,
    pub(crate) oracle: String,
    pub(crate) other: String,
}

/// Compares two sequences of backend frames. The sequence of message types must be the same,
/// and each pair of frames must be the same under its rule. A run of `ParameterStatus` frames is
/// compared as a set, because its order is the order of a hash table in PostgreSQL and no client
/// reads it.
pub(crate) fn frames(oracle: &[Frame], other: &[Frame]) -> Vec<Difference> {
    let oracle = group_parameters(oracle);
    let other = group_parameters(other);
    let mut out = Vec::new();
    for (i, pair) in oracle.iter().zip(&other).enumerate() {
        match pair {
            (Item::Frame(a), Item::Frame(b)) => {
                if a.tag != b.tag {
                    out.push(Difference {
                        at: i,
                        what: "message sequence".to_string(),
                        oracle: frame::backend_name(a.tag).to_string(),
                        other: frame::backend_name(b.tag).to_string(),
                    });
                    // After the sequence differs, every later pair is out of step.
                    return out;
                }
                if let Some(d) = one(a, b) {
                    out.push(Difference { at: i, ..d });
                }
            }
            (Item::Parameters(a), Item::Parameters(b)) => out.extend(parameters(i, a, b)),
            (a, b) => {
                out.push(Difference {
                    at: i,
                    what: "message sequence".to_string(),
                    oracle: a.name(),
                    other: b.name(),
                });
                return out;
            }
        }
    }
    if oracle.len() != other.len() {
        let name = |items: &[Item<'_>]| {
            items.get(oracle.len().min(other.len())).map_or("nothing".to_string(), Item::name)
        };
        out.push(Difference {
            at: oracle.len().min(other.len()),
            what: "message sequence".to_string(),
            oracle: name(&oracle),
            other: name(&other),
        });
    }
    out
}

/// Compares one pair of frames of the same type under the rule for that type.
fn one(a: &Frame, b: &Frame) -> Option<Difference> {
    let differ = |what: &str, x: String, y: String| {
        Some(Difference { at: 0, what: what.to_string(), oracle: x, other: y })
    };
    match a.tag {
        b'T' => {
            let (x, y) = (row_description(a), row_description(b));
            (x != y).then(|| differ("RowDescription", x, y)).flatten()
        }
        b'E' | b'N' => {
            let (x, y) = (notice(a), notice(b));
            (x != y).then(|| differ(frame::backend_name(a.tag), x, y)).flatten()
        }
        // The process ID and the key are random. Only the key length is part of the protocol.
        b'K' => (a.body.len() != b.body.len())
            .then(|| {
                differ(
                    "BackendKeyData key length",
                    (a.body.len() - 4).to_string(),
                    (b.body.len() - 4).to_string(),
                )
            })
            .flatten(),
        _ => (a.body != b.body)
            .then(|| differ(frame::backend_name(a.tag), show(&a.body), show(&b.body)))
            .flatten(),
    }
}

/// A `RowDescription` with the value of a non-zero table OID replaced by `user`, because a user
/// OID differs between servers. Zero against non-zero is still a difference: pgjdbc and sqlx use
/// the table OID to find the source column.
fn row_description(f: &Frame) -> String {
    let mut fields = f.fields();
    let Some(count) = fields.i16() else { return show(&f.body) };
    let mut out = format!("{count} fields");
    for _ in 0..count {
        let (
            Some(name),
            Some(table),
            Some(column),
            Some(ty),
            Some(size),
            Some(modifier),
            Some(format),
        ) = (
            fields.cstr(),
            fields.i32(),
            fields.i16(),
            fields.i32(),
            fields.i16(),
            fields.i32(),
            fields.i16(),
        )
        else {
            return show(&f.body);
        };
        let table = if table == 0 { "0" } else { "user" };
        let name = String::from_utf8_lossy(name);
        out.push_str(&format!("; {name:?} table {table} column {column} type {ty} size {size} modifier {modifier} format {format}"));
    }
    out
}

/// An `ErrorResponse` or `NoticeResponse` without the fields `F`, `L` and `R`, which name a C
/// file, line and routine in PostgreSQL. The other fields are sorted by code, because a client
/// reads a field by its code and not by its position.
fn notice(f: &Frame) -> String {
    let mut fields: Vec<(u8, String)> = frame::notice_fields(f)
        .into_iter()
        .filter(|(code, _)| !matches!(code, b'F' | b'L' | b'R'))
        .collect();
    fields.sort();
    fields
        .iter()
        .map(|(code, value)| format!("{}={value:?}", *code as char))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug)]
enum Item<'a> {
    Frame(&'a Frame),
    Parameters(Vec<(String, String)>),
}

impl Item<'_> {
    fn name(&self) -> String {
        match self {
            Item::Frame(f) => frame::backend_name(f.tag).to_string(),
            Item::Parameters(_) => "ParameterStatus".to_string(),
        }
    }
}

fn group_parameters(frames: &[Frame]) -> Vec<Item<'_>> {
    let mut out: Vec<Item<'_>> = Vec::new();
    for f in frames {
        if f.tag != b'S' {
            out.push(Item::Frame(f));
            continue;
        }
        let mut fields = f.fields();
        let name =
            fields.cstr().map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
        let value =
            fields.cstr().map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
        match out.last_mut() {
            Some(Item::Parameters(list)) => list.push((name, value)),
            _ => out.push(Item::Parameters(vec![(name, value)])),
        }
    }
    out
}

/// The parameters that the oracle reports must all be there with the same value, except the
/// version, which differs on purpose per document 02 section 2.5 of the notes. A parameter that
/// only the other server reports is a difference too, because a client sees it.
fn parameters(
    at: usize,
    oracle: &[(String, String)],
    other: &[(String, String)],
) -> Vec<Difference> {
    let free = |name: &str| matches!(name, "server_version" | "rudb.version");
    let mut out = Vec::new();
    for (name, value) in oracle {
        match other.iter().find(|(n, _)| n == name) {
            None => out.push(Difference {
                at,
                what: format!("ParameterStatus {name}"),
                oracle: value.clone(),
                other: "missing".to_string(),
            }),
            Some((_, v)) if v != value && !free(name) => out.push(Difference {
                at,
                what: format!("ParameterStatus {name}"),
                oracle: value.clone(),
                other: v.clone(),
            }),
            Some(_) => {}
        }
    }
    for (name, value) in other {
        if !oracle.iter().any(|(n, _)| n == name) && name != "rudb.version" {
            out.push(Difference {
                at,
                what: format!("ParameterStatus {name}"),
                oracle: "missing".to_string(),
                other: value.clone(),
            });
        }
    }
    out
}

/// Bytes for a person: text when they are text, hex when they are not.
pub(crate) fn show(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) if !s.chars().any(|c| c.is_control() && c != '\0') => {
            format!("{:?}", s.replace('\0', "\\0"))
        }
        _ => format!("0x{}", crate::crypto::hex(bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameter(name: &str, value: &str) -> Frame {
        Frame::new(b'S', format!("{name}\0{value}\0").into_bytes())
    }

    fn error(fields: &str) -> Frame {
        Frame::new(b'E', format!("{}\0", fields.replace('|', "\0")).into_bytes())
    }

    fn row_description(table: i32) -> Frame {
        let mut body = 1i16.to_be_bytes().to_vec();
        body.extend_from_slice(b"a\0");
        body.extend_from_slice(&table.to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&23i32.to_be_bytes());
        body.extend_from_slice(&4i16.to_be_bytes());
        body.extend_from_slice(&(-1i32).to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        Frame::new(b'T', body)
    }

    #[test]
    fn the_file_line_and_routine_of_an_error_are_not_compared() {
        let a = error(
            "SERROR|C42P01|Mrelation \"t\" does not exist|Fparse_relation.c|L1449|RparserOpenTable|",
        );
        let b = error("SERROR|C42P01|Mrelation \"t\" does not exist|Fbinder.rs|L12|");
        assert!(frames(std::slice::from_ref(&a), &[b]).is_empty());
        let c = error("SERROR|C42P01|Mrelation \"u\" does not exist|");
        assert_eq!(frames(&[a], &[c])[0].what, "ErrorResponse");
    }

    #[test]
    fn a_user_table_oid_is_compared_as_zero_or_not_zero() {
        assert!(frames(&[row_description(16384)], &[row_description(16999)]).is_empty());
        assert_eq!(frames(&[row_description(16384)], &[row_description(0)]).len(), 1);
    }

    #[test]
    fn parameters_are_a_set_and_the_version_is_free() {
        let a = [parameter("server_version", "19beta4"), parameter("TimeZone", "UTC")];
        let b = [parameter("TimeZone", "UTC"), parameter("server_version", "19.0 (rudb 0.9.0)")];
        assert!(frames(&a, &b).is_empty());
        let c = [parameter("TimeZone", "Etc/UTC"), parameter("server_version", "19beta4")];
        assert_eq!(frames(&a, &c)[0].what, "ParameterStatus TimeZone");
    }

    #[test]
    fn the_backend_key_is_compared_by_length() {
        let a = Frame::new(b'K', [1, 2, 3, 4, 5, 6, 7, 8].to_vec());
        let b = Frame::new(b'K', [9, 9, 9, 9, 8, 8, 8, 8].to_vec());
        assert!(frames(std::slice::from_ref(&a), &[b]).is_empty());
        let long = Frame::new(b'K', vec![0; 36]);
        assert_eq!(frames(&[a], &[long])[0].what, "BackendKeyData key length");
    }

    #[test]
    fn a_different_message_type_stops_the_comparison() {
        let a = [Frame::new(b'C', b"SELECT 1\0".to_vec()), Frame::new(b'Z', b"I".to_vec())];
        let b = [error("SERROR|C0A000|Mno|"), Frame::new(b'Z', b"E".to_vec())];
        let d = frames(&a, &b);
        assert_eq!(d.len(), 1);
        assert_eq!(
            (d[0].oracle.as_str(), d[0].other.as_str()),
            ("CommandComplete", "ErrorResponse")
        );
    }

    #[test]
    fn a_missing_frame_at_the_end_is_a_difference() {
        let a = [Frame::new(b'C', b"SELECT 1\0".to_vec()), Frame::new(b'Z', b"I".to_vec())];
        let d = frames(&a, &a[..1]);
        assert_eq!((d[0].oracle.as_str(), d[0].other.as_str()), ("ReadyForQuery", "nothing"));
    }
}
