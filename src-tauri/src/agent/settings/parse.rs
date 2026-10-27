//! Reading and writing one setting's value (the web's `_parse`,
//! `_serialise` and the `get_all` rendering).
//!
//! Strict on write, lenient on read: a request with a bad value is refused
//! with the web's sentence and changes nothing; a stored row that cannot be
//! read degrades to its default (or is clamped to its bound) so the order
//! guard never fails on a hand-edited row.

use super::decimal::{Finite, PyDecimal};
use super::pyrepr::{py_repr, py_str};
use super::schema::{Field, Kind, Shipped, MAX_SYSTEM_PROMPT_CHARS, SYSTEM_PROMPT};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// A setting's value in its typed form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    Bool(bool),
    Int(i64),
    Decimal(Finite),
    Text(String),
    Set(BTreeSet<String>),
}

const TRUE_TOKENS: &[&str] = &["1", "true", "t", "yes", "y", "on", "enabled"];
const FALSE_TOKENS: &[&str] = &["0", "false", "f", "no", "n", "off", "disabled"];

/// The shipped default of a field. `data_dir` places the kill switch file.
pub fn default_of(field: &Field, data_dir: &Path) -> Typed {
    match field.default {
        Shipped::Bool(b) => Typed::Bool(b),
        Shipped::Int(i) => Typed::Int(i),
        Shipped::Decimal(text) => match PyDecimal::parse(text) {
            Some(PyDecimal::Finite(f)) => Typed::Decimal(f),
            _ => Typed::Decimal(Finite::integer(0)),
        },
        Shipped::Text(t) => Typed::Text(t.to_string()),
        Shipped::Set(items) => Typed::Set(items.iter().map(|s| s.to_string()).collect()),
        Shipped::KillSwitchFile => Typed::Text(
            data_dir
                .join(super::schema::KILL_SWITCH_FILE_NAME)
                .to_string_lossy()
                .into_owned(),
        ),
    }
}

/// Parse a value: `strict` for a write (an error is the sentence the route
/// answers 400 with), lenient for a stored row (falls back to `default`).
/// `null` is the default, as the web's `_parse(None)` is.
pub fn parse(field: &Field, raw: &Value, default: &Typed, strict: bool) -> Result<Typed, String> {
    if raw.is_null() {
        return Ok(default.clone());
    }
    let key = field.key;
    let refuse = |message: String| -> Result<Typed, String> {
        if strict {
            Err(message)
        } else {
            tracing::warn!(
                "Agent setting {} holds an unusable value; using its default",
                key
            );
            Ok(default.clone())
        }
    };
    match field.kind {
        Kind::Bool => {
            if let Value::Bool(b) = raw {
                return Ok(Typed::Bool(*b));
            }
            let token = py_str(raw).trim().to_lowercase();
            if TRUE_TOKENS.contains(&token.as_str()) {
                Ok(Typed::Bool(true))
            } else if FALSE_TOKENS.contains(&token.as_str()) {
                Ok(Typed::Bool(false))
            } else {
                refuse(format!("{} must be a boolean, got {}", key, py_repr(raw)))
            }
        }
        Kind::Int => parse_int(field, raw, default, strict),
        Kind::Money | Kind::Percent => parse_decimal(field, raw, default, strict),
        Kind::Set => parse_set(field, raw, default, strict),
        Kind::Enum => {
            let token = py_str(raw).trim().to_lowercase();
            if field.choices.contains(&token.as_str()) {
                return Ok(Typed::Text(token));
            }
            let mut sorted: Vec<&str> = field.choices.to_vec();
            sorted.sort_unstable();
            refuse(format!(
                "{} must be one of {}, got {}",
                key,
                sorted.join(", "),
                py_repr(raw)
            ))
        }
        Kind::Text => {
            let text = py_str(raw);
            if strict
                && key == SYSTEM_PROMPT
                && text.trim().chars().count() > MAX_SYSTEM_PROMPT_CHARS
            {
                return Err(SYSTEM_PROMPT_TOO_LONG.to_string());
            }
            Ok(Typed::Text(text))
        }
    }
}

/// D-014: the override is capped where the trader saves it, rather than
/// dropped whole by the prompt budget at the next message.
pub const SYSTEM_PROMPT_TOO_LONG: &str = "The system prompt can be at most 20,000 characters. A longer one does not fit in the agent's instructions, so shorten it and save it again.";

/// Python's `int(str(raw).strip())`: a sign, digits, single underscores.
fn whole_number(text: &str) -> Option<i128> {
    let (negative, rest) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if rest.is_empty() || rest.starts_with('_') || rest.ends_with('_') || rest.contains("__") {
        return None;
    }
    let digits: String = rest.chars().filter(|c| *c != '_').collect();
    if digits.len() > 30 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let value: i128 = digits.parse().ok()?;
    Some(if negative { -value } else { value })
}

fn parse_int(field: &Field, raw: &Value, default: &Typed, strict: bool) -> Result<Typed, String> {
    let key = field.key;
    let Some(value) = whole_number(py_str(raw).trim()) else {
        if strict {
            return Err(format!(
                "{} must be a whole number, got {}",
                key,
                py_repr(raw)
            ));
        }
        tracing::warn!(
            "Agent setting {} holds an unparseable integer; using its default",
            key
        );
        return Ok(default.clone());
    };
    // The bounds the schema states, then the range a stored integer can
    // hold (Python's are unbounded; a larger one is no limit at all).
    let min = i128::from(field.min.unwrap_or(i64::MIN));
    let max = i128::from(field.max.unwrap_or(i64::MAX));
    if value < min {
        if strict {
            return Err(format!("{} must be at least {}, got {}", key, min, value));
        }
        return Ok(Typed::Int(min as i64));
    }
    if value > max {
        if strict {
            return Err(format!("{} must be at most {}, got {}", key, max, value));
        }
        return Ok(Typed::Int(max as i64));
    }
    Ok(Typed::Int(value as i64))
}

fn parse_decimal(
    field: &Field,
    raw: &Value,
    default: &Typed,
    strict: bool,
) -> Result<Typed, String> {
    let key = field.key;
    let fallback = |message: String| -> Result<Typed, String> {
        if strict {
            Err(message)
        } else {
            tracing::warn!(
                "Agent setting {} holds an unusable number; using its default",
                key
            );
            Ok(default.clone())
        }
    };
    let value = match PyDecimal::parse(py_str(raw).trim()) {
        None => return fallback(format!("{} must be a number, got {}", key, py_repr(raw))),
        Some(PyDecimal::Special(_)) => {
            return fallback(format!(
                "{} must be a finite number, got {}",
                key,
                py_repr(raw)
            ))
        }
        Some(PyDecimal::Finite(f)) => f,
    };
    let value = if field.kind == Kind::Money {
        // Money is quantised to paise. More digits than Python's context
        // holds raises there (an HTTP 500 on the web); here it is refused
        // as not a usable number.
        match value.quantize_cents() {
            Some(v) => v,
            None => return fallback(format!("{} must be a number, got {}", key, py_repr(raw))),
        }
    } else {
        value
    };
    if let Some(min) = field.min {
        let bound = Finite::integer(min.max(0) as u64);
        if value.compare(&bound).is_lt() {
            if strict {
                return Err(format!("{} must be at least {}, got {}", key, bound, value));
            }
            return Ok(Typed::Decimal(bound));
        }
    }
    if let Some(max) = field.max {
        let bound = Finite::integer(max.max(0) as u64);
        if value.compare(&bound).is_gt() {
            if strict {
                return Err(format!("{} must be at most {}, got {}", key, bound, value));
            }
            return Ok(Typed::Decimal(bound));
        }
    }
    Ok(Typed::Decimal(value))
}

fn parse_set(field: &Field, raw: &Value, default: &Typed, strict: bool) -> Result<Typed, String> {
    let key = field.key;
    let fallback = |message: String| -> Result<Typed, String> {
        if strict {
            Err(message)
        } else {
            tracing::warn!(
                "Agent setting {} holds an unusable list; using its default",
                key
            );
            Ok(default.clone())
        }
    };
    let items: Vec<Value> = match raw {
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(Typed::Set(BTreeSet::new()));
            }
            if text.starts_with('[') {
                match serde_json::from_str::<Value>(text) {
                    Ok(Value::Array(items)) => items,
                    Ok(_) => return fallback(format!("{} must be a JSON array", key)),
                    Err(_) => {
                        return fallback(format!(
                            "{} must be a JSON array or a comma-separated list",
                            key
                        ))
                    }
                }
            } else {
                text.split(',')
                    .map(|s| Value::String(s.to_string()))
                    .collect()
            }
        }
        Value::Array(items) => items.clone(),
        // A mapping iterates its keys in Python.
        Value::Object(map) => map.keys().map(|k| Value::String(k.clone())).collect(),
        other => {
            return fallback(format!(
                "{} must be a list of strings, got {}",
                key,
                py_repr(other)
            ))
        }
    };
    Ok(Typed::Set(
        items
            .iter()
            .map(|item| py_str(item).trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect(),
    ))
}

/// The text stored in `ag_setting.value` (the web's `_serialise`).
pub fn serialise(value: &Typed) -> String {
    match value {
        Typed::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Typed::Int(i) => i.to_string(),
        Typed::Decimal(d) => d.to_string(),
        Typed::Text(t) => t.clone(),
        Typed::Set(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|s| json_ascii(s))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A JSON string as Python's `json.dumps` writes it (`ensure_ascii`).
fn json_ascii(s: &str) -> String {
    let plain = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    let mut out = String::with_capacity(plain.len());
    for c in plain.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{:04x}", unit));
            }
        }
    }
    out
}

/// The value as `GET /agent/api/settings` shows it: decimals as strings,
/// sets as sorted lists.
pub fn render(value: &Typed) -> Value {
    match value {
        Typed::Bool(b) => Value::Bool(*b),
        Typed::Int(i) => Value::from(*i),
        Typed::Decimal(d) => Value::String(d.to_string()),
        Typed::Text(t) => Value::String(t.clone()),
        Typed::Set(items) => Value::Array(items.iter().cloned().map(Value::String).collect()),
    }
}
