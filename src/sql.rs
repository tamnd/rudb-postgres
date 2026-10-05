//! Splits a SQL file into statements, the way psql does.
//!
//! The splitter knows quoted strings, `E''` strings, quoted identifiers, dollar quotes, line
//! comments and nested block comments, and a semicolon inside any of them does not end a
//! statement. A line that starts with a backslash is a psql meta-command. The splitter keeps it
//! as its own item, so that a runner can count it as not run. After `COPY ... FROM stdin`, the
//! lines up to `\.` are the data of that statement, as in a psql script.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    /// A statement without its final semicolon, and the line it starts on.
    Statement { line: usize, text: String, copy_data: Option<String> },
    /// A psql meta-command, which the harness does not run.
    Meta { line: usize, text: String },
}

pub(crate) fn split(text: &str) -> Vec<Item> {
    let bytes = text.as_bytes();
    let mut items = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let mut start = 0;
    let mut start_line = 1;
    let mut at_line_start = true;
    let mut empty = true;

    while i < bytes.len() {
        let b = bytes[i];
        if at_line_start && empty && b == b'\\' {
            let end = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
            items.push(Item::Meta { line, text: text[i..end].trim_end().to_string() });
            i = end;
            start = i;
            continue;
        }
        at_line_start = b == b'\n';
        match b {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let (end, lines) = block_comment(bytes, i);
                line += lines;
                i = end;
            }
            b'\'' | b'"' => {
                if empty {
                    (start, start_line, empty) = (i, line, false);
                }
                let escapes = b == b'\''
                    && i > 0
                    && matches!(bytes[i - 1], b'E' | b'e')
                    && !ident_byte(bytes.get(i.wrapping_sub(2)));
                let (end, lines) = quoted(bytes, i, b, escapes);
                line += lines;
                i = end;
            }
            b'$' if !ident_byte(bytes.get(i.wrapping_sub(1))) && dollar_tag(bytes, i).is_some() => {
                if empty {
                    (start, start_line, empty) = (i, line, false);
                }
                let tag = dollar_tag(bytes, i).unwrap_or_default();
                let body = i + tag.len();
                let end = text[body..].find(&tag).map_or(bytes.len(), |n| body + n + tag.len());
                line += text[i..end].matches('\n').count();
                i = end;
            }
            b';' => {
                if !empty {
                    let statement = text[start..i].trim().to_string();
                    i += 1;
                    let copy_data = if is_copy_from_stdin(&statement) {
                        let rest = &text[i..];
                        let data_start = rest.find('\n').map_or(rest.len(), |n| n + 1);
                        let mut data = String::new();
                        let mut consumed = data_start;
                        for data_line in rest[data_start..].split_inclusive('\n') {
                            consumed += data_line.len();
                            line += 1;
                            if data_line.trim_end_matches(['\n', '\r']) == "\\." {
                                break;
                            }
                            data.push_str(data_line);
                        }
                        line += 1;
                        i += consumed;
                        at_line_start = true;
                        Some(data)
                    } else {
                        None
                    };
                    items.push(Item::Statement { line: start_line, text: statement, copy_data });
                } else {
                    i += 1;
                }
                empty = true;
                start = i;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            _ => {
                if empty {
                    (start, start_line, empty) = (i, line, false);
                }
                i += 1;
            }
        }
    }
    if !empty {
        let statement = text[start..].trim().to_string();
        if !statement.is_empty() {
            items.push(Item::Statement { line: start_line, text: statement, copy_data: None });
        }
    }
    items
}

fn ident_byte(b: Option<&u8>) -> bool {
    b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b >= 0x80)
}

/// The tag of a dollar quote that starts at `i`, for example `$$` or `$fn$`.
fn dollar_tag(bytes: &[u8], i: usize) -> Option<String> {
    let mut j = i + 1;
    while j < bytes.len()
        && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] >= 0x80)
    {
        if j == i + 1 && bytes[j].is_ascii_digit() {
            // $1 is a parameter, not a quote.
            return None;
        }
        j += 1;
    }
    (bytes.get(j) == Some(&b'$')).then(|| String::from_utf8_lossy(&bytes[i..=j]).into_owned())
}

/// The end of a quoted string or identifier that starts at `i`, and the lines in it. A doubled
/// quote is a quote. With `escapes`, a backslash quotes the next byte.
fn quoted(bytes: &[u8], i: usize, quote: u8, escapes: bool) -> (usize, usize) {
    let mut j = i + 1;
    let mut lines = 0;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' if escapes => j += 1,
            b'\n' => lines += 1,
            b if b == quote => {
                if bytes.get(j + 1) == Some(&quote) {
                    j += 1;
                } else {
                    return (j + 1, lines);
                }
            }
            _ => {}
        }
        j += 1;
    }
    (bytes.len(), lines)
}

/// The end of a block comment that starts at `i`, and the lines in it. Block comments nest.
fn block_comment(bytes: &[u8], i: usize) -> (usize, usize) {
    let mut depth = 0;
    let mut j = i;
    let mut lines = 0;
    while j < bytes.len() {
        if bytes[j] == b'/' && bytes.get(j + 1) == Some(&b'*') {
            depth += 1;
            j += 2;
        } else if bytes[j] == b'*' && bytes.get(j + 1) == Some(&b'/') {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return (j, lines);
            }
        } else {
            lines += usize::from(bytes[j] == b'\n');
            j += 1;
        }
    }
    (bytes.len(), lines)
}

fn is_copy_from_stdin(statement: &str) -> bool {
    let words: Vec<String> = statement.split_whitespace().map(str::to_ascii_lowercase).collect();
    words.first().is_some_and(|w| w == "copy")
        && words.windows(2).any(|w| w[0] == "from" && w[1].trim_end_matches(';') == "stdin")
}

/// Whether a statement has an `ORDER BY` outside any parentheses, strings and comments. Rows
/// are compared in order only then, per document 01 section 1.5 of the notes.
pub(crate) fn has_top_level_order_by(statement: &str) -> bool {
    let bytes = statement.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, words: &mut Vec<String>, depth: i32| {
        if !word.is_empty() {
            if depth == 0 {
                words.push(word.to_ascii_lowercase());
            }
            word.clear();
        }
    };
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\'' | b'"' => {
                flush(&mut word, &mut words, depth);
                i = quoted(bytes, i, b, false).0;
                continue;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                flush(&mut word, &mut words, depth);
                i = statement[i..].find('\n').map_or(bytes.len(), |n| i + n);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                flush(&mut word, &mut words, depth);
                i = block_comment(bytes, i).0;
                continue;
            }
            b'$' if dollar_tag(bytes, i).is_some() => {
                flush(&mut word, &mut words, depth);
                let tag = dollar_tag(bytes, i).unwrap_or_default();
                let body = i + tag.len();
                i = statement[body..].find(&tag).map_or(bytes.len(), |n| body + n + tag.len());
                continue;
            }
            b'(' => {
                flush(&mut word, &mut words, depth);
                depth += 1;
            }
            b')' => {
                flush(&mut word, &mut words, depth);
                depth -= 1;
            }
            b if b.is_ascii_alphanumeric() || b == b'_' => word.push(b as char),
            _ => flush(&mut word, &mut words, depth),
        }
        i += 1;
    }
    flush(&mut word, &mut words, depth);
    words.windows(2).any(|w| w[0] == "order" && w[1] == "by")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sql: &str) -> Vec<String> {
        split(sql)
            .into_iter()
            .map(|item| match item {
                Item::Statement { text, .. } => text,
                Item::Meta { text, .. } => format!("META {text}"),
            })
            .collect()
    }

    #[test]
    fn semicolons_in_quotes_and_comments_do_not_split() {
        let sql = "select 'a;b', \"c;d\"; -- x;y\nselect /* p; /* q; */ r; */ 1;\nselect E'\\';';";
        assert_eq!(
            texts(sql),
            ["select 'a;b', \"c;d\"", "select /* p; /* q; */ r; */ 1", "select E'\\';'"]
        );
    }

    #[test]
    fn dollar_quotes_hold_semicolons_and_parameters_are_not_quotes() {
        let sql =
            "create function f() returns int as $fn$ select 1; $fn$ language sql;\nselect $1::int;";
        assert_eq!(
            texts(sql),
            [
                "create function f() returns int as $fn$ select 1; $fn$ language sql",
                "select $1::int"
            ]
        );
    }

    #[test]
    fn meta_commands_are_their_own_items() {
        let sql = "\\set VERBOSITY terse\nselect 1;\n\\d t\n";
        assert_eq!(texts(sql), ["META \\set VERBOSITY terse", "select 1", "META \\d t"]);
    }

    #[test]
    fn copy_from_stdin_takes_the_data_up_to_the_terminator() {
        let sql = "copy t from stdin;\n1\ta\n2\tb\n\\.\nselect 2;\n";
        let items = split(sql);
        assert_eq!(items.len(), 2);
        let Item::Statement { copy_data, .. } = &items[0] else { panic!("not a statement") };
        assert_eq!(copy_data.as_deref(), Some("1\ta\n2\tb\n"));
        assert_eq!(items[1], Item::Statement { line: 5, text: "select 2".into(), copy_data: None });
    }

    #[test]
    fn line_numbers_follow_the_file() {
        let items = split("select 1;\n\n/* a\nb */\nselect\n 2;");
        assert!(matches!(&items[1], Item::Statement { line: 5, .. }));
    }

    #[test]
    fn order_by_counts_only_at_the_top_level() {
        assert!(has_top_level_order_by("select a from t order by a"));
        assert!(!has_top_level_order_by("select * from (select a from t order by a) s"));
        assert!(!has_top_level_order_by("select 'order by' from t"));
        assert!(!has_top_level_order_by("select array_agg(a order by a) from t"));
    }
}
