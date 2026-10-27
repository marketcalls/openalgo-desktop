//! How an Agent request is refused before anything streams: the status and
//! the web's error envelope, `{"status": "error", "message": ..., ...}`.

use serde_json::{json, Value};

/// A request refused before its stream opened: the status and the web's
/// error envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub status: u16,
    pub body: Value,
}

impl Refusal {
    pub fn new(status: u16, message: impl Into<String>, extra: Option<Value>) -> Refusal {
        let mut body = json!({"status": "error", "message": message.into()});
        if let (Some(Value::Object(extra)), Some(obj)) = (extra, body.as_object_mut()) {
            obj.extend(extra);
        }
        Refusal { status, body }
    }

    pub fn bad_request(message: impl Into<String>) -> Refusal {
        Refusal::new(400, message, None)
    }

    pub fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }
}

/// The web's `NOT_FOUND`: a conversation that is not the trader's reads
/// as absent, never as forbidden, so ids cannot be probed.
pub const NOT_FOUND: &str = "Conversation not found";
