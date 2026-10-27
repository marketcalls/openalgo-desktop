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
    "symboltoken",
    "symbol_token",
    "instrument_token",
    "exchange_token",
    "brtoken",
];

/// Value shapes that are secrets whatever they are called: a provider key,
/// a bearer token, a JWT (a broker's access token is often one), a long
/// pure-hex string (every generated key here).
static SECRET_VALUES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\bsk-[A-Za-z0-9_\-]{16,}",
        r"(?i)\bBearer\s+[A-Za-z0-9._\-]{16,}",
        r"\beyJ[A-Za-z0-9_\-]{4,}\.[A-Za-z0-9_\-]{4,}\.[A-Za-z0-9_\-]*",
        r"\b[A-Za-z0-9_\-]{16,}\.[A-Za-z0-9_\-]{16,}\.[A-Za-z0-9_\-]{16,}",
        r"(?i)\b[0-9a-f]{32,}\b",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

/// A run of base64 or base64url characters long enough to be a key.
static LONG_RUN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/_\-]{40,}={0,2}").ok());

/// A long run that mixes upper case, lower case and digits is random key
/// material, not a word, a symbol or a number.
fn looks_random(run: &str) -> bool {
    run.chars().any(|c| c.is_ascii_uppercase())
        && run.chars().any(|c| c.is_ascii_lowercase())
        && run.chars().any(|c| c.is_ascii_digit())
}

/// Whether a key's value must not be shown or stored. A bare `token` is
/// judged by its value ([`redact_token`]): it names an instrument as often
/// as a credential.
pub fn is_secret_key(key: &str) -> bool {
    let lowered = key.trim().to_lowercase();
    if SAFE_KEY_NAMES.contains(&lowered.as_str()) {
        return false;
    }
    SECRET_KEY_MARKERS.iter().any(|m| lowered.contains(m))
}

/// The longest instrument token kept under a bare `token` key.
const MAX_INSTRUMENT_TOKEN_DIGITS: usize = 12;

/// A bare `token` or `tokens` field: kept only when it is an instrument
/// token (a short run of digits, or such a number); anything else is
/// treated as the credential it may be (security review, 2026-10-27: the
/// web safelisted the name and let a broker auth token through).
fn redact_token(value: &Value) -> Value {
    let short_digits = |s: &str| {
        !s.is_empty()
            && s.len() <= MAX_INSTRUMENT_TOKEN_DIGITS
            && s.chars().all(|c| c.is_ascii_digit())
    };
    match value {
        Value::Number(n) if n.is_u64() && short_digits(&n.to_string()) => value.clone(),
        Value::String(s) if short_digits(s.trim()) => value.clone(),
        Value::Array(items) => {
            Value::Array(items.iter().take(MAX_ITEMS).map(redact_token).collect())
        }
        Value::Null => Value::Null,
        _ => Value::String(REDACTED.into()),
    }
}

/// Remove secret-shaped substrings and cap the length.
pub fn redact_text(value: &str) -> String {
    let mut text = value.to_string();
    for pattern in SECRET_VALUES.iter() {
        text = pattern.replace_all(&text, REDACTED).into_owned();
    }
    if let Some(re) = LONG_RUN.as_ref() {
        text = re
            .replace_all(&text, |c: &regex::Captures<'_>| {
                let run = c.get(0).map(|m| m.as_str()).unwrap_or("");
                if looks_random(run) {
                    REDACTED.to_string()
                } else {
                    run.to_string()
                }
            })
            .into_owned();
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
                    let bare = k.trim().to_lowercase();
                    let v = if bare == "token" || bare == "tokens" {
                        redact_token(v)
                    } else if is_secret_key(k) {
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

    /// Security review, 2026-10-27: the web safelisted a bare `token`, so a
    /// broker's auth token in a field of that name went to the model and
    /// the audit unmasked.
    #[test]
    fn a_bare_token_is_kept_only_when_it_is_an_instrument_token() {
        // Assembled here so the secret scan does not read the source as a key.
        let auth = ["a1b2c3", "d4e5f6"].concat();
        let r = redact(&json!({
            "token": auth,
            "tokens": ["2885", "x-auth-123"],
            "Token": 2885,
            "symboltoken": "3045",
            "instrument_token": 408065,
        }));
        assert_eq!(r["token"], REDACTED);
        assert_eq!(r["tokens"], json!(["2885", REDACTED]));
        assert_eq!(r["Token"], 2885);
        assert_eq!(r["symboltoken"], "3045");
        assert_eq!(r["instrument_token"], 408065);
        assert_eq!(redact(&json!({"token": "2885"}))["token"], "2885");
    }

    #[test]
    fn a_jwt_or_long_random_key_is_removed_from_any_field() {
        // Assembled here so the secret scan does not read the source as a key.
        let jwt = [
            "eyJhbGciOiJIUzI1NiJ9",
            "eyJzdWIiOiJBQjEyMzQifQ",
            "c2lnbmF0dXJlLXZhbHVl",
        ]
        .join(".");
        let run = ["Zm9vYmFyQmF6UXV4", "MTIzNDU2Nzg5MEFC", "Q0RFRkdISUpLTE1O"].concat();
        let r = redact(&json!({
            "message": format!("session {} expired", jwt),
            "data": {"note": format!("key={} done", run)},
            "hex": "id 0123456789abcdef0123456789abcdef end",
            "symbol": "NIFTY28MAR2420800CE",
            "path": "/agent/api/conversations/12/messages/40",
        }));
        assert_eq!(r["message"], "session [redacted] expired");
        assert_eq!(r["data"]["note"], "key=[redacted] done");
        assert_eq!(r["hex"], "id [redacted] end");
        assert_eq!(r["symbol"], "NIFTY28MAR2420800CE");
        assert_eq!(r["path"], "/agent/api/conversations/12/messages/40");
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
