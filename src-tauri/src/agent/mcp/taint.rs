//! The taint boundary for external data arguments (the web's design 55,
//! "The taint boundary"; D-021: the web passes them through as the model
//! wrote them, so a model steered by injected text could place account data
//! in one).
//!
//! Every argument sent to a server is built out of words that are already
//! the trader's or the server's own, never out of anything the model
//! invented or copied from a tool result: each whitespace-delimited word of
//! a text value, and the text of a number, must appear (ignoring case) in
//! the trader's message for this turn, or be a name or a value the tool's
//! own `inputSchema` lists (its argument names, an `enum` or a `default`),
//! which the server already knows. A key must be one of those too. A switch
//! or null carries no account data and passes. Credential-shaped text is
//! refused before anything is compared.
//!
//! The web's search boundary keeps the words that pass and drops the rest.
//! An argument is a lookup value, not a query: "State Bank of" names a
//! different thing from "State Bank of India". So a value with a word that
//! does not pass is not trimmed; the call is refused, and the model is told
//! to use the trader's words or ask. What is sent is the words, joined by
//! single spaces. The cost is a value the model worked out itself (a date
//! from "last month", a count the trader did not say), which it has to ask
//! for; nothing is sent until it does.

use crate::agent::redact::redact_text;
use serde_json::{Map, Value};
use std::collections::HashSet;

/// How deep a value is followed; deeper is refused.
const MAX_DEPTH: usize = 6;

/// The words an argument may be built from.
pub struct Vocabulary {
    /// The trader's message, credentials removed, lower case.
    operator: String,
    /// Names and values the tool's schema lists, lower case.
    server: HashSet<String>,
}

impl Vocabulary {
    pub fn new(operator_message: &str, schema: Option<&Value>) -> Vocabulary {
        let mut server = HashSet::new();
        if let Some(s) = schema {
            collect(s, &mut server, 0);
        }
        Vocabulary {
            operator: redact_text(operator_message).to_lowercase(),
            server,
        }
    }

    fn allows(&self, word: &str) -> bool {
        let w = word.to_lowercase();
        self.server.contains(&w) || (!w.is_empty() && self.operator.contains(&w))
    }
}

/// The property names, enum values and defaults of a schema.
fn collect(schema: &Value, out: &mut HashSet<String>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let Some(obj) = schema.as_object() else {
        return;
    };
    if let Some(props) = obj.get("properties").and_then(Value::as_object) {
        for (name, prop) in props {
            out.insert(name.to_lowercase());
            collect(prop, out, depth + 1);
        }
    }
    for listed in obj
        .get("enum")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(word) = scalar_text(listed) {
            out.insert(word.to_lowercase());
        }
    }
    if let Some(word) = obj.get("default").and_then(scalar_text) {
        out.insert(word.to_lowercase());
    }
    for key in ["items", "anyOf", "oneOf"] {
        match obj.get(key) {
            Some(Value::Array(items)) => items.iter().for_each(|i| collect(i, out, depth + 1)),
            Some(item) => collect(item, out, depth + 1),
            None => {}
        }
    }
}

fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// An argument that may not leave: where it is and the word that stopped
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub at: String,
    pub word: String,
}

fn refuse(at: &str, word: &str) -> Refused {
    Refused {
        at: at.to_string(),
        word: word.chars().take(40).collect(),
    }
}

/// The arguments as they will be sent, every word the trader's or the
/// server's, or the first one that is not.
pub fn construct(
    args: &Map<String, Value>,
    vocab: &Vocabulary,
) -> Result<Map<String, Value>, Refused> {
    let mut out = Map::new();
    for (key, value) in args {
        if !vocab.allows(key) {
            return Err(refuse(key, key));
        }
        out.insert(key.clone(), value_of(value, vocab, key, 1)?);
    }
    Ok(out)
}

fn value_of(value: &Value, vocab: &Vocabulary, at: &str, depth: usize) -> Result<Value, Refused> {
    if depth > MAX_DEPTH {
        return Err(refuse(at, "(nested too deeply)"));
    }
    match value {
        Value::Null | Value::Bool(_) => Ok(value.clone()),
        Value::Number(n) => {
            let text = n.to_string();
            if vocab.allows(&text) {
                Ok(value.clone())
            } else {
                Err(refuse(at, &text))
            }
        }
        Value::String(s) => {
            if redact_text(s) != *s {
                return Err(refuse(at, "(a value shaped like a key)"));
            }
            let words: Vec<&str> = s.split_whitespace().collect();
            match words.iter().find(|w| !vocab.allows(w)) {
                Some(word) => Err(refuse(at, word)),
                None => Ok(Value::String(words.join(" "))),
            }
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, v)| value_of(v, vocab, &format!("{}[{}]", at, i), depth + 1))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let here = format!("{}.{}", at, k);
                if !vocab.allows(k) {
                    return Err(refuse(&here, k));
                }
                out.insert(k.clone(), value_of(v, vocab, &here, depth + 1)?);
            }
            Ok(Value::Object(out))
        }
    }
}
