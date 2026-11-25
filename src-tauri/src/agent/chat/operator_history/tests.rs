//! The web's `test/test_agent_composer_backend.py` history cases: 5e19ce407
//! (only the trader's rows, bounded, never failing) and 1ed72181e (each
//! message redacted whole before anything is cut).

use super::{from_rows, read, MAX_CHARS};
use crate::agent::conversations::store::Message;
use serde_json::json;

fn row(role: &str, content: &str) -> Message {
    Message {
        id: 0,
        conversation_id: 1,
        role: role.into(),
        content: content.into(),
        tools: json!([]),
        notices: json!([]),
        created_at: String::new(),
    }
}

fn typed(contents: &[&str]) -> Vec<Message> {
    contents.iter().map(|c| row("user", c)).collect()
}

/// A fake key, assembled at run time so no scanner reads it as one.
fn secret() -> String {
    ["sk-live-", "Qm4Zr9Tx", "Lw2VbNc8", "Yd5HsKj7"].concat()
}

/// Every run of at least six characters of the key found in the text.
fn fragments(secret: &str, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for i in 0..secret.len() {
        for j in (i + 6)..=secret.len() {
            if text.contains(&secret[i..j]) {
                found.push(secret[i..j].to_string());
            }
        }
    }
    found
}

/// Web `test_only_what_the_operator_typed_is_offered_as_history`: an
/// assistant reply can quote a web page.
#[test]
fn only_what_the_trader_typed_is_offered_as_history() {
    let rows = vec![
        row("user", "live quotes of reliance"),
        row(
            "assistant",
            "Reliance is at 1,410 per this page https://evil.example",
        ),
        row("user", "and get the marketcap of it"),
    ];
    let history = from_rows(&rows);
    assert_eq!(
        history,
        "live quotes of reliance\nand get the marketcap of it"
    );
    assert!(!history.contains("evil.example"));
}

/// Web `test_history_is_bounded_in_messages_and_characters_and_keeps_the_newest`.
#[test]
fn history_is_bounded_and_keeps_the_newest() {
    let contents: Vec<String> = (0..20)
        .map(|i| format!("message {} {}", i, "x".repeat(900)))
        .collect();
    let rows: Vec<Message> = contents.iter().map(|c| row("user", c)).collect();
    let history = from_rows(&rows);
    assert!(history.chars().count() <= MAX_CHARS);
    assert!(history.contains("message 19") && !history.contains("message 0 "));
}

/// Web `test_history_never_raises_where_nothing_may`: a store that cannot
/// be read is no history, never a failed run.
#[test]
fn an_unreadable_store_is_no_history() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    assert_eq!(read(&conn, 1), "");
}

/// Web `test_an_empty_conversation_adds_no_history_key`.
#[test]
fn an_empty_conversation_has_no_history() {
    assert_eq!(from_rows(&[]), "");
    assert_eq!(from_rows(&typed(&["   "])), "");
}

/// Web `test_a_cut_cannot_leave_the_tail_of_a_credential_behind` (1ed72181e):
/// with the cut a few characters into a key, its prefix was gone and the
/// rest matched no pattern, so it survived as "words".
#[test]
fn a_cut_cannot_leave_the_tail_of_a_credential_behind() {
    let key = secret();
    let first = format!("my key is {} ok", key);
    let cut_into_the_key = "my key is sk-live-Qm4Z".len();
    let newer: String = "reliance "
        .repeat(600)
        .chars()
        .take(MAX_CHARS + cut_into_the_key - first.len() - 1)
        .collect();
    let history = from_rows(&typed(&[&first, &newer]));
    assert_eq!(fragments(&key, &history), Vec::<String>::new());
}

/// Web `test_a_credential_in_a_kept_message_is_redacted_before_it_leaves_the_route`.
#[test]
fn a_credential_in_a_kept_message_is_redacted() {
    let key = secret();
    let history = from_rows(&typed(&[&format!(
        "api_key={} and the company is reliance",
        key
    )]));
    assert_eq!(fragments(&key, &history), Vec::<String>::new());
    assert!(
        history.contains("reliance"),
        "the rest of the message is still the trader's words"
    );
}

/// Web `test_a_credential_split_across_words_is_redacted_in_the_whole_message`.
#[test]
fn a_credential_after_its_label_is_redacted_in_the_whole_message() {
    let key = secret();
    let history = from_rows(&typed(&[&format!(
        "password: {}{} then reliance",
        &key[..12],
        &key[12..]
    )]));
    assert_eq!(fragments(&key, &history), Vec::<String>::new());
}

/// Web `test_one_message_over_the_budget_is_cut_at_whitespace_never_inside_a_word`.
#[test]
fn one_message_over_the_budget_is_cut_at_white_space_never_inside_a_word() {
    let words: Vec<String> = (0..1200).map(|i| format!("word{:04}", i)).collect();
    let history = from_rows(&typed(&[&words.join(" ")]));
    let n = history.chars().count();
    assert!(n > 0 && n <= MAX_CHARS, "{}", n);
    assert!(
        history
            .split_whitespace()
            .all(|w| words.iter().any(|x| x == w)),
        "every token is a whole word"
    );
    assert!(
        history.ends_with("word1199"),
        "the newest end is the part kept"
    );
}

/// Web `test_a_secret_in_a_message_over_the_budget_is_redacted_before_it_is_cut`.
#[test]
fn a_secret_in_a_message_over_the_budget_is_redacted_before_it_is_cut() {
    let key = secret();
    let big = format!(
        "{}my key is {} {}",
        "reliance ".repeat(500),
        key,
        "reliance ".repeat(100)
    );
    let history = from_rows(&typed(&[&big]));
    assert_eq!(fragments(&key, &history), Vec::<String>::new());
}
