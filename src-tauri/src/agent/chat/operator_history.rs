//! The trader's earlier messages in a conversation, for the web tools (the
//! web's `_operator_history`, 5e19ce407, with the redaction rule of
//! 1ed72181e).
//!
//! A follow-up such as "get the marketcap of it" names its company only in
//! an earlier message; without these words the search query lost it and
//! the page for it was refused. Only the trader's own rows qualify: a turn
//! stores the typed text, never the composed input, so an attached file is
//! not in them, and an assistant reply, which can quote a web page, is not
//! either. Bounded on purpose (the last eight, at most 4,000 characters):
//! it bounds what an old message can contribute, not what the model reads.
//!
//! Each message is redacted whole before anything is cut. Redaction knows
//! a credential by its prefix, its length and the words around it, so a cut
//! made first can leave the tail of a key that no pattern knows, and that
//! tail would then be a word the trader "typed". The text is only ever
//! shortened by dropping whole oldest messages, and one message longer
//! than the budget is cut at white space, never inside a word. The web
//! shipped the cut-first form in 5e19ce407 and fixed it in 1ed72181e; the
//! desktop never had it.

use crate::agent::conversations::store::{self, Message};
use crate::agent::websearch::taint::redact;
use crate::agent::words::is_space;
use rusqlite::Connection;

/// How many earlier messages may give words (`MAX_OPERATOR_HISTORY_MESSAGES`).
pub const MAX_MESSAGES: usize = 8;
/// How many characters of them (`MAX_OPERATOR_HISTORY_CHARS`).
pub const MAX_CHARS: usize = 4000;
/// The newest rows read to find them (the web's `limit=64`).
const ROWS_READ: i64 = 64;

/// The trader's earlier messages in `conversation`, newest last; empty
/// when there is none or the store cannot be read (a run is still built:
/// its tools then take words from the turn's message alone).
pub fn read(conn: &Connection, conversation: i64) -> String {
    match store::list_messages(conn, conversation, ROWS_READ) {
        Ok(rows) => from_rows(&rows),
        Err(e) => {
            tracing::error!(
                "Could not read the earlier messages of conversation {}: {}",
                conversation,
                e
            );
            String::new()
        }
    }
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

/// The history out of a conversation's rows, oldest first.
pub fn from_rows(rows: &[Message]) -> String {
    let typed: Vec<&str> = rows
        .iter()
        .filter(|r| r.role == "user")
        .map(|r| r.content.as_str())
        .collect();
    let newest = &typed[typed.len().saturating_sub(MAX_MESSAGES)..];
    let mut kept: Vec<String> = newest
        .iter()
        .map(|t| redact(t).trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    while kept.len() > 1 && chars(&kept.join("\n")) > MAX_CHARS {
        kept.remove(0);
    }
    let joined = kept.join("\n");
    if chars(&joined) <= MAX_CHARS {
        return joined;
    }
    newest_words(&joined)
}

/// The last [`MAX_CHARS`] characters of one message over the budget,
/// without the partial word the cut landed in.
fn newest_words(text: &str) -> String {
    let all: Vec<char> = text.chars().collect();
    let cut = all.len().saturating_sub(MAX_CHARS);
    let tail: String = all[cut..].iter().collect();
    if cut == 0 || is_space(all[cut - 1]) {
        return tail;
    }
    match tail.trim_start_matches(is_space).split_once(is_space) {
        Some((_, rest)) => rest.trim_start_matches(is_space).to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests;
