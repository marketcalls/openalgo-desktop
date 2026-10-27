//! The trader's models and keys (the web's model registry: the model
//! routes of `blueprints/agent.py` over `database/agent_db.py`).
//!
//! - `migration`: migration `098_agent_config` (the three tables).
//! - `store`: `ag_provider_model` rows and the default rules.
//! - `secrets`: `ag_secret`, sealed under the app's data key.
//! - `service`: what the model routes do: register, edit, remove, make
//!   default, list, each answering with the web's body and status.
//!
//! The answers are the web's envelope: `{"status": "success", ...}`, or a
//! [`Refused`] the route turns into `{"status": "error", "message": ...}`.

pub mod migration;
pub mod secrets;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use serde_json::{Map, Value};

/// A request refused with the web's status and sentence, and any extra
/// fields its error body carries (`data` for a model saved without its key).
#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    pub status: u16,
    pub message: String,
    pub extra: Option<Value>,
}

impl Refused {
    pub fn new(status: u16, message: impl Into<String>) -> Refused {
        Refused {
            status,
            message: message.into(),
            extra: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Refused {
        Refused::new(400, message)
    }

    /// The error body (the web's `_error`).
    pub fn body(&self) -> Value {
        let mut out = Map::new();
        out.insert("status".into(), "error".into());
        out.insert("message".into(), self.message.clone().into());
        if let Some(Value::Object(extra)) = &self.extra {
            out.extend(extra.clone());
        }
        Value::Object(out)
    }
}

/// The success body (the web's `_ok`): the payload's keys beside
/// `"status": "success"`.
pub fn success(payload: Value) -> Value {
    let mut out = Map::new();
    out.insert("status".into(), "success".into());
    if let Value::Object(fields) = payload {
        out.extend(fields);
    }
    Value::Object(out)
}

/// A database error named by its kind only: an error's text can quote the
/// values it failed on, and a secret path must not log one.
pub(crate) fn error_kind(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(f, _) => format!("database error {:?}", f.code),
        _ => "database error".into(),
    }
}

/// A request field as the web reads it, `str(body.get(k) or "").strip()`:
/// an absent, null, false, zero or empty value is empty.
pub(crate) fn text_field(body: &Map<String, Value>, key: &str) -> String {
    match body.get(key) {
        None => String::new(),
        Some(v) if !truthy(v) => String::new(),
        Some(v) => crate::agent::settings::pyrepr::py_str(v).trim().to_string(),
    }
}

/// Python's truthiness of a JSON value (`bool(value)`).
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
