//! Parse acceptance: whether rudb and the oracle agree on which statements the grammar accepts,
//! per document 08 section 8.15 of the notes and the first condition of the PG4 gate.
//!
//! Each statement is sent to a session held in a failed transaction block. `exec_simple_query`
//! parses all of a query before it checks the block, so a statement that the grammar refuses gets
//! the error of the grammar, and any other statement gets 25P02 and does not run. So the two
//! servers answer for their grammars only, the corpus can be any SQL, and nothing changes on
//! either server. A statement that ends the block, such as `ROLLBACK`, runs. The session then
//! fails a block again before the next statement.

use std::path::Path;

use crate::client::{Client, Login};
use crate::frame::{self, Frame};
use crate::json::Json;
use crate::servers::Server;
use crate::sql::{self, Item};

/// The SQLSTATE of a statement in a failed block.
const IN_FAILED_BLOCK: &str = "25P02";
/// The class of the savepoint errors. `ROLLBACK TO` is one of the statements that run in a failed
/// block, and with no such savepoint it fails after the grammar accepted it.
const SAVEPOINT_CLASS: &str = "3B";

/// What the grammar of a server did with a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    Accepted,
    /// Refused, with the SQLSTATE and the message.
    Refused(String, String),
}

/// One session in a failed block.
struct Session {
    client: Client,
}

impl Session {
    fn open(server: &Server) -> Result<Session, String> {
        let client = Client::connect(server, &Login::superuser("postgres"))
            .map_err(|refused| format!("cannot connect to {}: {}", server.name, refused.message))?;
        let mut session = Session { client };
        session.fail()?;
        Ok(session)
    }

    /// Opens a block and fails it.
    fn fail(&mut self) -> Result<(), String> {
        for sql in ["begin", "select 1/0"] {
            self.client.simple(sql).map_err(|e| format!("cannot send {sql:?}: {e}"))?;
        }
        Ok(())
    }

    fn answer(&mut self, statement: &str) -> Result<Answer, String> {
        let frames = self.client.simple(statement).map_err(|e| format!("lost the session: {e}"))?;
        let answer = match frames.iter().find(|f| f.tag == b'E') {
            Some(error) => {
                let (code, message) = code_and_message(error);
                answer_of(code, message)
            }
            None => Answer::Accepted,
        };
        // A statement that ends the block leaves the session out of a failed block.
        if frames.last().and_then(|z| z.body.first()) != Some(&b'E') {
            self.fail()?;
        }
        Ok(answer)
    }
}

/// What an error in a failed block says about the grammar.
fn answer_of(code: String, message: String) -> Answer {
    if code == IN_FAILED_BLOCK || code.starts_with(SAVEPOINT_CLASS) {
        Answer::Accepted
    } else {
        Answer::Refused(code, message)
    }
}

fn code_and_message(error: &Frame) -> (String, String) {
    let fields = frame::notice_fields(error);
    let get = |code: u8| {
        fields.iter().find(|(c, _)| *c == code).map(|(_, v)| v.clone()).unwrap_or_default()
    };
    (get(b'C'), get(b'M'))
}

/// Runs the statements of each file on both servers and counts where they agree.
pub(crate) fn run(
    oracle: &Server,
    other: &Server,
    files: &[&Path],
) -> Result<(Json, usize), String> {
    let mut sessions = (Session::open(oracle)?, Session::open(other)?);
    let (mut statements, mut accepted, mut same_code) = (0usize, 0usize, 0usize);
    let mut differences = Vec::new();
    let mut skipped = Vec::new();
    for file in files {
        let bytes =
            std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        // A file in another encoding, such as `collate.windows.win1252.sql`, is for a database in
        // that encoding, and both servers are UTF-8.
        let Ok(text) = String::from_utf8(bytes) else {
            skipped.push(Json::str(&file.display().to_string()));
            continue;
        };
        let name = file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        for item in sql::split(&text) {
            let Item::Statement { line, text, .. } = item else { continue };
            statements += 1;
            let a = sessions.0.answer(&text)?;
            let b = sessions.1.answer(&text)?;
            let agree = matches!(
                (&a, &b),
                (Answer::Accepted, Answer::Accepted) | (Answer::Refused(..), Answer::Refused(..))
            );
            if agree {
                accepted += 1;
                match (&a, &b) {
                    (Answer::Refused(x, _), Answer::Refused(y, _)) if x != y => {}
                    _ => same_code += 1,
                }
                continue;
            }
            let show = |answer: &Answer| match answer {
                Answer::Accepted => Json::str("accepted"),
                Answer::Refused(code, message) => Json::str(&format!("{code} {message}")),
            };
            differences.push(Json::object(vec![
                ("file", Json::str(&name)),
                ("line", Json::Number(line as i64)),
                ("statement", Json::str(&text)),
                ("oracle", show(&a)),
                ("rudb", show(&b)),
            ]));
        }
    }
    let count = differences.len();
    let percent = if statements == 0 { 100.0 } else { accepted as f64 * 100.0 / statements as f64 };
    println!(
        "parse acceptance: {accepted} of {statements} statements agree ({percent:.2} percent), \
         {same_code} with the same SQLSTATE"
    );
    let json = Json::object(vec![
        ("statements", Json::Number(statements as i64)),
        ("agree", Json::Number(accepted as i64)),
        ("same_sqlstate", Json::Number(same_code as i64)),
        ("percent", Json::Float(percent)),
        ("skipped", Json::Array(skipped)),
        ("differences", Json::Array(differences)),
    ]);
    Ok((json, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_block_and_a_savepoint_error_mean_the_grammar_accepted() {
        let refused = |code: &str| answer_of(code.to_owned(), String::new());
        assert_eq!(refused("25P02"), Answer::Accepted);
        assert_eq!(refused("3B001"), Answer::Accepted);
        assert_eq!(refused("42601"), Answer::Refused("42601".to_owned(), String::new()));
        assert_eq!(refused("0A000"), Answer::Refused("0A000".to_owned(), String::new()));
    }
}
