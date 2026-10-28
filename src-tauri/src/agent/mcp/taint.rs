//! The taint boundary for external data arguments (the web's design 55,
//! "The taint boundary"; D-021: the web passes them through as the model
//! wrote them, so a model steered by injected text could place account data
//! in one).
//!
//! Every argument sent to a server is built out of what is already the
//! trader's or the server's own, never out of anything the model invented
//! or copied from a tool result (security reviews, 2026-10-28):
//!
//! - **The trader's words.** The trader's message for this turn is redacted
//!   (credentials out) and split once on white space, each word stripped of
//!   edge punctuation, the redaction marker removed. A word of a text value,
//!   and a number's text, must be one of those words, whole and ignoring
//!   case (a fragment, a piece of the marker, or a digit inside a longer
//!   number does not pass), and is sent as the trader wrote it, never in
//!   the model's casing. Each word is used at most as many times as the
//!   trader wrote it, and all the words together at most as many as the
//!   message has.
//! - **The server's literals.** Or the whole value is a literal the tool's
//!   `inputSchema` lists for that argument (an `enum` value or the `const`).
//! - **The schema's shape.** A key is an argument name the schema declares,
//!   never a trader's word. Each value's JSON type must be one the schema
//!   declares for it (`type`, and `items.type` for a list); a switch or a
//!   null is refused where the schema does not declare one. A list holds at
//!   most [`MAX_LITERAL_ITEMS`] switches or literals.
//! - **Few short pieces.** One- and two-character words, switches and nulls
//!   are what data would be spelled out in: more than
//!   [`MAX_SHORT_PIECES`] across the whole call is refused. A nested key
//!   counts as one of the trader's words.
//!
//! What is left (which of these words, in what order) can still carry a few
//! bits; `docs/security/known-residuals.md` records it.
//!
//! The web's search boundary keeps the words that pass and drops the rest.
//! An argument is a lookup value, not a query: "State Bank of" names a
//! different thing from "State Bank of India". So nothing is trimmed: the
//! call is refused, the model is told why and to use the trader's words or
//! ask, and nothing is sent until it does.

use crate::agent::redact::{redact_secrets, REDACTED};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// How deep a value is followed; deeper is refused.
const MAX_DEPTH: usize = 6;

/// Most one- and two-character words, switches and nulls in one call.
pub const MAX_SHORT_PIECES: usize = 3;

/// Most switches or literals in one list.
pub const MAX_LITERAL_ITEMS: usize = 8;

/// Stripped from a word's ends before it is compared (the web's
/// `_EDGE_PUNCTUATION`, as the web search boundary strips it).
const EDGE: &[char] = &[
    ' ', '\t', '\n', '\r', '"', '\'', '`', '.', ',', ';', ':', '!', '?', '(', ')', '[', ']', '{',
    '}', '<', '>', '|', '\\', '/', '*', '~', '#',
];

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c| EDGE.contains(&c)))
        .filter(|w| !w.is_empty())
}

fn is_short(text: &str) -> bool {
    text.chars().count() <= 2
}

/// The trader's words: each one's own form and how often it was written.
pub struct Vocabulary {
    trader: HashMap<String, (String, usize)>,
    total: usize,
}

impl Vocabulary {
    pub fn new(operator_message: &str) -> Vocabulary {
        let marker = REDACTED.trim_matches(|c| EDGE.contains(&c)).to_lowercase();
        let redacted = redact_secrets(operator_message);
        let mut trader: HashMap<String, (String, usize)> = HashMap::new();
        let mut total = 0;
        for w in words(&redacted).filter(|w| w.to_lowercase() != marker) {
            trader
                .entry(w.to_lowercase())
                .or_insert_with(|| (w.to_string(), 0))
                .1 += 1;
            total += 1;
        }
        Vocabulary { trader, total }
    }
}

/// Why an argument may not leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// A word that is neither the trader's nor a literal of the tool's.
    NotTheTraders,
    /// A word used more often than the trader wrote it, or more words than
    /// the message has.
    TooOften,
    /// A key the schema does not declare.
    NotDeclared,
    /// A value of a type the schema does not declare for it.
    WrongType,
    /// More short words, switches and nulls than [`MAX_SHORT_PIECES`].
    ShortPieces,
    /// A list with more switches or literals than [`MAX_LITERAL_ITEMS`].
    TooManyItems,
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
        let word = || crate::agent::tools::service::py_repr(&Value::String(self.word.clone()));
        match self.why {
            Why::NotTheTraders => format!("{} holds words the trader did not write in this message ({}), and an external server is sent only the trader's own words or the values its tool lists.", self.at, word()),
            Why::TooOften => format!("{} uses {} more often than the trader wrote it, and an external server is sent only the trader's own words.", self.at, word()),
            Why::NotDeclared => format!("{} is not an argument this tool lists.", self.at),
            Why::WrongType => format!("{} is not of the type this tool lists for it.", self.at),
            Why::ShortPieces => format!("{} brings the call to more than {} one- and two-character words, switches or nulls, which is how data is spelled out a piece at a time, so it is not sent.", self.at, MAX_SHORT_PIECES),
            Why::TooManyItems => format!("{} lists more than {} switches or fixed values.", self.at, MAX_LITERAL_ITEMS),
            Why::KeyShaped => format!("{} looks like a key or a password, which is never sent to an external server.", self.at),
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

/// The JSON types a schema declares for one value (`type`, as a name or a
/// list, and those of its `anyOf`/`oneOf` branches); `None` when it states
/// none.
fn types(schema: Option<&Value>) -> Option<Vec<&str>> {
    let s = schema?;
    let mut out: Vec<&str> = match s.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    for key in ["anyOf", "oneOf"] {
        for branch in s.get(key).and_then(Value::as_array).into_iter().flatten() {
            out.extend(types(Some(branch)).unwrap_or_default());
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Whether a value is of a type the schema declares. With none declared,
/// anything but a switch or a null.
fn typed(value: &Value, schema: Option<&Value>) -> bool {
    let Some(declared) = types(schema) else {
        return !matches!(value, Value::Bool(_) | Value::Null);
    };
    declared.iter().any(|t| match (*t, value) {
        ("string", Value::String(_))
        | ("number", Value::Number(_))
        | ("boolean", Value::Bool(_))
        | ("null", Value::Null)
        | ("array", Value::Array(_))
        | ("object", Value::Object(_)) => true,
        ("integer", Value::Number(n)) => {
            n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        _ => false,
    })
}

/// The schema of a named property, of an object schema.
fn property<'a>(schema: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    schema?.get("properties")?.get(name)
}

/// The arguments as they will be sent, every piece the trader's or a
/// literal of the tool's, or the first one that is not. `schema` is the
/// tool's `inputSchema`.
pub fn construct(
    args: &Map<String, Value>,
    vocab: &Vocabulary,
    schema: Option<&Value>,
) -> Result<Map<String, Value>, Refused> {
    let mut builder = Builder {
        vocab,
        used: HashMap::new(),
        words: 0,
        short: 0,
    };
    let mut out = Map::new();
    for (key, value) in args {
        let Some(declared) = property(schema, key) else {
            return Err(refuse(key, key, Why::NotDeclared));
        };
        out.insert(key.clone(), builder.value(value, Some(declared), key, 1)?);
    }
    Ok(out)
}

/// One call's construction, with what it has used so far.
struct Builder<'a> {
    vocab: &'a Vocabulary,
    /// How often each trader word has been used.
    used: HashMap<String, usize>,
    /// Trader words used, nested keys included.
    words: usize,
    /// One- and two-character words, switches and nulls.
    short: usize,
}

impl Builder<'_> {
    fn short_piece(&mut self, at: &str) -> Result<(), Refused> {
        self.short += 1;
        if self.short > MAX_SHORT_PIECES {
            return Err(refuse(at, "", Why::ShortPieces));
        }
        Ok(())
    }

    /// One trader word, in the trader's own form.
    fn word(&mut self, at: &str, word: &str) -> Result<String, Refused> {
        let lower = word.to_lowercase();
        let Some((own, written)) = self.vocab.trader.get(&lower) else {
            return Err(refuse(at, word, Why::NotTheTraders));
        };
        let used = self.used.entry(lower).or_insert(0);
        if *used >= *written || self.words >= self.vocab.total {
            return Err(refuse(at, word, Why::TooOften));
        }
        *used += 1;
        self.words += 1;
        if is_short(own) {
            self.short_piece(at)?;
        }
        Ok(own.clone())
    }

    fn value(
        &mut self,
        value: &Value,
        schema: Option<&Value>,
        at: &str,
        depth: usize,
    ) -> Result<Value, Refused> {
        if depth > MAX_DEPTH {
            return Err(refuse(at, "", Why::TooDeep));
        }
        if !matches!(value, Value::Array(_) | Value::Object(_)) && literals(schema).contains(&value)
        {
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if is_short(&text) || matches!(value, Value::Bool(_) | Value::Null) {
                self.short_piece(at)?;
            }
            return Ok(value.clone());
        }
        if !typed(value, schema) {
            return Err(refuse(at, "", Why::WrongType));
        }
        match value {
            Value::Null | Value::Bool(_) => {
                self.short_piece(at)?;
                Ok(value.clone())
            }
            Value::Number(n) => {
                self.word(at, &n.to_string())?;
                Ok(value.clone())
            }
            Value::String(s) => {
                if redact_secrets(s) != *s {
                    return Err(refuse(at, "", Why::KeyShaped));
                }
                let own = words(s)
                    .map(|w| self.word(at, w))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::String(own.join(" ")))
            }
            Value::Array(items) => self.list(items, schema, at, depth),
            Value::Object(map) => {
                let mut out = Map::new();
                for (k, v) in map {
                    let here = format!("{}.{}", at, k);
                    let Some(declared) = property(schema, k) else {
                        return Err(refuse(&here, k, Why::NotDeclared));
                    };
                    // A nested key is a piece too: it counts as a word.
                    if self.words >= self.vocab.total {
                        return Err(refuse(&here, k, Why::TooOften));
                    }
                    self.words += 1;
                    out.insert(k.clone(), self.value(v, Some(declared), &here, depth + 1)?);
                }
                Ok(Value::Object(out))
            }
        }
    }

    fn list(
        &mut self,
        items: &[Value],
        schema: Option<&Value>,
        at: &str,
        depth: usize,
    ) -> Result<Value, Refused> {
        let inner = schema.and_then(|s| s.get("items"));
        let fixed = items
            .iter()
            .filter(|v| matches!(v, Value::Bool(_) | Value::Null) || literals(inner).contains(v))
            .count();
        if fixed > MAX_LITERAL_ITEMS {
            return Err(refuse(at, "", Why::TooManyItems));
        }
        items
            .iter()
            .enumerate()
            .map(|(i, v)| self.value(v, inner, &format!("{}[{}]", at, i), depth + 1))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)
    }
}
