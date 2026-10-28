//! The taint boundary for external data arguments (the web's design 55,
//! "The taint boundary"; D-021: the web passes them through as the model
//! wrote them, so a model steered by injected text could place account data
//! in one).
//!
//! Every argument sent to a server is built out of words that are already
//! the trader's or the server's own, never out of anything the model
//! invented or copied from a tool result:
//!
//! - The trader's message for this turn is redacted (credentials out),
//!   split once on white space, each word stripped of edge punctuation and
//!   lower-cased, the redaction marker removed: that set is the trader's
//!   words.
//! - Each word of a text value, edge punctuation stripped, must be exactly
//!   one of the trader's words; a number's text likewise. A fragment of a
//!   word, of the marker, or a letter or digit inside a longer word does
//!   not pass (security review, 2026-10-28: the web's search boundary tests
//!   a substring of the message, and data could leave a piece at a time).
//!   The web search boundary uses the same exact-word rule.
//! - Or the whole value is a literal the tool's `inputSchema` lists for that
//!   argument (an `enum` value or the `const`), which the server already
//!   knows.
//! - A key is an argument name the schema declares, or one of the trader's
//!   words. A switch or null carries no account data and passes.
//! - An argument built mostly from one- or two-character words is refused:
//!   letters and digits are the pieces data would be spelled out in.
//!
//! The web's search boundary keeps the words that pass and drops the rest.
//! An argument is a lookup value, not a query: "State Bank of" names a
//! different thing from "State Bank of India". So a value with a word that
//! does not pass is not trimmed; the call is refused, and the model is told
//! to use the trader's words or ask. What is sent is the words, joined by
//! single spaces. The cost is a value the model worked out itself (a date
//! from "last month", a count the trader did not say), which it has to ask
//! for; nothing is sent until it does.

use crate::agent::redact::{redact_secrets, REDACTED};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// How deep a value is followed; deeper is refused.
const MAX_DEPTH: usize = 6;

/// Stripped from a word's ends before it is compared (the web's
/// `_EDGE_PUNCTUATION`, as the web search boundary strips it).
const EDGE: &[char] = &[
    ' ', '\t', '\n', '\r', '"', '\'', '`', '.', ',', ';', ':', '!', '?', '(', ')', '[', ']', '{',
    '}', '<', '>', '|', '\\', '/', '*', '~', '#',
];

/// A word of at most this many characters is a short one.
const SHORT_WORD_CHARS: usize = 2;

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c| EDGE.contains(&c)))
        .filter(|w| !w.is_empty())
}

/// The trader's words: what an argument may be built from.
pub struct Vocabulary {
    trader: HashSet<String>,
}

impl Vocabulary {
    pub fn new(operator_message: &str) -> Vocabulary {
        let marker = REDACTED.trim_matches(|c| EDGE.contains(&c)).to_lowercase();
        Vocabulary {
            trader: words(&redact_secrets(operator_message))
                .map(str::to_lowercase)
                .filter(|w| *w != marker)
                .collect(),
        }
    }

    fn has(&self, word: &str) -> bool {
        self.trader.contains(&word.to_lowercase())
    }
}

/// The literal values (`enum`, `const`) a schema lists for one value.
fn literals(schema: Option<&Value>) -> Vec<&Value> {
    let Some(s) = schema else { return Vec::new() };
    let mut out: Vec<&Value> = s
        .get("enum")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    out.extend(s.get("const"));
    for key in ["anyOf", "oneOf"] {
        for branch in s.get(key).and_then(Value::as_array).into_iter().flatten() {
            out.extend(literals(Some(branch)));
        }
    }
    out
}

/// The schema of a named property, of an object schema.
fn property<'a>(schema: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    schema?.get("properties")?.get(name)
}

fn declares(schema: Option<&Value>, name: &str) -> bool {
    property(schema, name).is_some()
}

/// Why an argument may not leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// A word that is neither the trader's nor a literal of the tool's.
    NotTheTraders,
    /// Mostly one- and two-character words.
    ShortWords,
    /// Shaped like a key or a password.
    KeyShaped,
    /// Nested past what is followed.
    TooDeep,
}

/// An argument that may not leave: where it is, the word that stopped it
/// and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub at: String,
    pub word: String,
    pub why: Why,
}

impl Refused {
    /// The correction the model reads.
    pub fn reason(&self) -> String {
        match self.why {
            Why::NotTheTraders => format!(
                "{} holds words the trader did not write in this message ({}), and an external server is sent only the trader's own words or the values its tool lists.",
                self.at,
                crate::agent::tools::service::py_repr(&Value::String(self.word.clone()))
            ),
            Why::ShortWords => format!(
                "{} is made mostly of one- and two-character words, which is how data is spelled out a piece at a time, so it is not sent.",
                self.at
            ),
            Why::KeyShaped => format!(
                "{} looks like a key or a password, which is never sent to an external server.",
                self.at
            ),
            Why::TooDeep => format!("{} is nested too deeply.", self.at),
        }
    }
}

fn refuse(at: &str, word: &str, why: Why) -> Refused {
    Refused {
        at: at.to_string(),
        word: word.chars().take(40).collect(),
        why,
    }
}

/// The arguments as they will be sent, every word the trader's or a
/// literal of the tool's, or the first one that is not. `schema` is the
/// tool's `inputSchema`.
pub fn construct(
    args: &Map<String, Value>,
    vocab: &Vocabulary,
    schema: Option<&Value>,
) -> Result<Map<String, Value>, Refused> {
    let mut out = Map::new();
    for (key, value) in args {
        if !declares(schema, key) && !vocab.has(key) {
            return Err(refuse(key, key, Why::NotTheTraders));
        }
        let built = value_of(value, vocab, property(schema, key), key, 1)?;
        let mut tally = (0usize, 0usize);
        count_words(&built, &mut tally);
        let (all, short) = tally;
        if all >= 2 && short * 2 > all {
            return Err(refuse(key, "", Why::ShortWords));
        }
        out.insert(key.clone(), built);
    }
    Ok(out)
}

/// `(words, short words)` of a built value, the literals included.
fn count_words(value: &Value, tally: &mut (usize, usize)) {
    let mut add = |w: &str| {
        tally.0 += 1;
        if w.chars().count() <= SHORT_WORD_CHARS {
            tally.1 += 1;
        }
    };
    match value {
        Value::String(s) => words(s).for_each(&mut add),
        Value::Number(n) => add(&n.to_string()),
        Value::Array(items) => items.iter().for_each(|v| count_words(v, tally)),
        Value::Object(map) => map.values().for_each(|v| count_words(v, tally)),
        Value::Null | Value::Bool(_) => {}
    }
}

fn value_of(
    value: &Value,
    vocab: &Vocabulary,
    schema: Option<&Value>,
    at: &str,
    depth: usize,
) -> Result<Value, Refused> {
    if depth > MAX_DEPTH {
        return Err(refuse(at, "", Why::TooDeep));
    }
    if !matches!(value, Value::Array(_) | Value::Object(_)) && literals(schema).contains(&value) {
        return Ok(value.clone());
    }
    match value {
        Value::Null | Value::Bool(_) => Ok(value.clone()),
        Value::Number(n) => {
            let text = n.to_string();
            if vocab.has(&text) {
                Ok(value.clone())
            } else {
                Err(refuse(at, &text, Why::NotTheTraders))
            }
        }
        Value::String(s) => {
            if redact_secrets(s) != *s {
                return Err(refuse(at, "", Why::KeyShaped));
            }
            let kept: Vec<&str> = words(s).collect();
            match kept.iter().find(|w| !vocab.has(w)) {
                Some(word) => Err(refuse(at, word, Why::NotTheTraders)),
                None => Ok(Value::String(kept.join(" "))),
            }
        }
        Value::Array(items) => {
            let inner = schema.and_then(|s| s.get("items"));
            items
                .iter()
                .enumerate()
                .map(|(i, v)| value_of(v, vocab, inner, &format!("{}[{}]", at, i), depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let here = format!("{}.{}", at, k);
                if !declares(schema, k) && !vocab.has(k) {
                    return Err(refuse(&here, k, Why::NotTheTraders));
                }
                out.insert(
                    k.clone(),
                    value_of(v, vocab, property(schema, k), &here, depth + 1)?,
                );
            }
            Ok(Value::Object(out))
        }
    }
}
