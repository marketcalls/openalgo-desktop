//! The bytes of the streamable-HTTP transport (the web's `parse_sse`,
//! `_pick_response` and `_content_text`): a reply's body read as JSON or as
//! a short event stream, the response to one request picked out of it, and
//! a tool result's content joined into text.

use serde_json::{Map, Value};
use std::collections::HashMap;

/// One HTTP reply: plain data, header names lower-cased, the body decoded
/// as UTF-8 (a tool description carries the rupee sign, and an event stream
/// with no charset must not be read in a guessed encoding).
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub text: String,
    /// The body ran past what the client reads; it is not used.
    pub oversized: bool,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// The JSON messages a `text/event-stream` body carries: events split by a
/// blank line, each event's `data:` lines joined with newlines; comments,
/// `event:` and `id:` lines ignored, an event that is not JSON skipped.
pub fn parse_sse(text: &str) -> Vec<Value> {
    let normal = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut messages = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    let mut flush = |data: &mut Vec<&str>| {
        if data.is_empty() {
            return;
        }
        match serde_json::from_str(&data.join("\n")) {
            Ok(v) => messages.push(v),
            Err(_) => tracing::debug!("Skipped a non-JSON server-sent event"),
        }
        data.clear();
    };
    for line in normal.split('\n') {
        if line.is_empty() {
            flush(&mut data);
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    flush(&mut data);
    messages
}

/// The response to request `id` among a stream's messages (a batch is
/// flattened): the message with our id carrying a result or an error,
/// else the last message carrying either.
pub fn pick_response(messages: Vec<Value>, id: u64) -> Option<Map<String, Value>> {
    let flat: Vec<Map<String, Value>> = messages
        .into_iter()
        .flat_map(|m| match m {
            Value::Array(items) => items,
            other => vec![other],
        })
        .filter_map(|m| match m {
            Value::Object(o) => Some(o),
            _ => None,
        })
        .collect();
    let answers = |m: &Map<String, Value>| m.contains_key("result") || m.contains_key("error");
    if let Some(at) = flat
        .iter()
        .position(|m| m.get("id") == Some(&Value::from(id)) && answers(m))
    {
        return flat.into_iter().nth(at);
    }
    flat.into_iter().rev().find(answers)
}

/// A `tools/call` result's content items as text: text items joined with
/// newlines, a resource's text in place, anything else named.
pub fn content_text(content: Option<&Value>) -> String {
    let Some(Value::Array(items)) = content else {
        return String::new();
    };
    items
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|item| match item.get("type").and_then(Value::as_str) {
            Some("text") => Some(
                item.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            Some("resource") => Some(
                item.get("resource")
                    .and_then(|r| r.get("text"))
                    .and_then(Value::as_str)
                    .map_or_else(
                        || "[a binary resource was omitted]".to_string(),
                        str::to_string,
                    ),
            ),
            Some(kind) if !kind.is_empty() => Some(format!("[{} content was omitted]", kind)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
