//! Secrets out of anything the Agent shows, stores or logs (the web's
//! `services/agent/safety/audit.py:redact`).
//!
//! Walks a value, replacing the value of any key whose name looks like a
//! credential and any string that looks like a key, and caps depth, list
//! length and string length. Used on tool arguments and results before
//! they reach a frame, an audit row or the log.

use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;

/// What a removed value reads as.
pub const REDACTED: &str = "[redacted]";

const MAX_STRING: usize = 2000;
const MAX_ITEMS: usize = 200;
const MAX_DEPTH: usize = 6;

/// Substrings of a lower-cased key that mark a credential.
const SECRET_KEY_MARKERS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "secret",
    "token",
    "password",
    "passwd",
    "pwd",
    "credential",
    "authorization",
    "auth_token",
    "cookie",
    "pepper",
    "private",
    "passphrase",
    "totp",
];

/// Names that contain a marker but are not secrets (token counts, an
/// instrument's token).
const SAFE_KEY_NAMES: &[&str] = &[
    "max_tokens",
    "min_tokens",
    "num_tokens",
    "n_tokens",
    "token_count",
    "total_tokens",
    "input_tokens",
    "output_tokens",
    "prompt_tokens",
    "completion_tokens",
    "reasoning_tokens",
    "cached_tokens",
    "tokens_used",
    "token",
    "tokens",
    "symboltoken",
    "symbol_token",
    "instrument_token",
    "exchange_token",
    "brtoken",
];

/// Value shapes that are secrets whatever they are called: a provider key,
/// a bearer token, a long pure-hex string (every generated key here).
static SECRET_VALUES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\bsk-[A-Za-z0-9_\-]{16,}",
        r"(?i)\bBearer\s+[A-Za-z0-9._\-]{16,}",
        r"(?i)^[0-9a-f]{32,}$",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

/// Whether a key's value must not be shown or stored.
pub fn is_secret_key(key: &str) -> bool {
    let lowered = key.trim().to_lowercase();
    if SAFE_KEY_NAMES.contains(&lowered.as_str()) {
        return false;
    }
    SECRET_KEY_MARKERS.iter().any(|m| lowered.contains(m))
}

/// Remove secret-shaped substrings and cap the length.
pub fn redact_text(value: &str) -> String {
    let mut text = value.to_string();
    for pattern in SECRET_VALUES.iter() {
        text = pattern.replace_all(&text, REDACTED).into_owned();
    }
    let chars = text.chars().count();
    if chars > MAX_STRING {
        let head: String = text.chars().take(MAX_STRING).collect();
        return format!("{}... [truncated, {} characters]", head, chars);
    }
    text
}

/// A copy of `value` with every secret removed and every size bounded.
pub fn redact(value: &Value) -> Value {
    walk(value, 0)
}

/// [`redact`] for an argument mapping.
pub fn redact_map(args: &Map<String, Value>) -> Map<String, Value> {
    match walk(&Value::Object(args.clone()), 0) {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn walk(value: &Value, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return Value::String("[nested too deeply]".into());
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(s) => Value::String(redact_text(s)),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if is_secret_key(k) {
                        Value::String(REDACTED.into())
                    } else {
                        walk(v, depth + 1)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(items) => {
            let mut out: Vec<Value> = items
                .iter()
                .take(MAX_ITEMS)
                .map(|v| walk(v, depth + 1))
                .collect();
            if items.len() > MAX_ITEMS {
                out.push(Value::String(format!(
                    "... [{} more items]",
                    items.len() - MAX_ITEMS
                )));
            }
            Value::Array(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credential_keys_and_shapes_are_removed() {
        let v = json!({
            "apikey": "abc",
            "X-API-KEY": "abc",
            "broker_secret": "s",
            "max_tokens": 5,
            "symboltoken": "3045",
            "note": "use sk-ABCDEFGHIJKLMNOPQRST now",
            "auth": "Bearer abcdefghijklmnopqrstu",
            "hex": "0123456789abcdef0123456789abcdef",
        });
        let r = redact(&v);
        assert_eq!(r["apikey"], REDACTED);
        assert_eq!(r["X-API-KEY"], REDACTED);
        assert_eq!(r["broker_secret"], REDACTED);
        assert_eq!(r["max_tokens"], 5);
        assert_eq!(r["symboltoken"], "3045");
        assert_eq!(r["note"], "use [redacted] now");
        assert_eq!(r["auth"], REDACTED);
        assert_eq!(r["hex"], REDACTED);
    }

    #[test]
    fn sizes_are_bounded_as_the_web_bounds_them() {
        let long = "x".repeat(2500);
        assert_eq!(
            redact_text(&long),
            format!("{}... [truncated, 2500 characters]", "x".repeat(2000))
        );
        let many: Vec<Value> = (0..205).map(Value::from).collect();
        let r = redact(&Value::Array(many));
        assert_eq!(r.as_array().unwrap().len(), 201);
        assert_eq!(r[200], "... [5 more items]");
        let mut deep = json!(1);
        for _ in 0..8 {
            deep = json!([deep]);
        }
        assert!(serde_json::to_string(&redact(&deep))
            .unwrap()
            .contains("[nested too deeply]"));
    }
}
