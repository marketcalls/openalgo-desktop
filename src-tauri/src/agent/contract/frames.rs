//! The Agent's stream frames, wire-exact with the web's
//! `services/agent/frames.py`.
//!
//! Every byte a chat stream writes is produced here. One frame is one SSE
//! data event, `data: {json}\n\n`, with no `event:` line: the client reads
//! the stream with `fetch` and a `ReadableStream` (the request carries a
//! body) and switches on the `type` field. Fields go out in the web's
//! declaration order, compact, with non-ASCII text left as it is.
//!
//! A run that pauses for approval ends its stream with a [`Frame::Confirm`]
//! and no [`Frame::Done`] after it; the client must not read the missing
//! done frame as a failure.
//!
//! The contract test below replays `tests/fixtures/agent/frames.json`,
//! recorded from the web by `scripts/agent/capture_web_frames.py`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Response headers of the streaming routes. `X-Accel-Buffering` stops a
/// reverse proxy holding frames back. `Connection` is deliberately absent:
/// the server owns it, and a `keep-alive` on a stream the server closes
/// produced `Connection: keep-alive, close` on the web.
pub const SSE_HEADERS: [(&str, &str); 3] = [
    ("Content-Type", "text/event-stream"),
    ("Cache-Control", "no-cache"),
    ("X-Accel-Buffering", "no"),
];

/// An SSE comment that keeps an idle connection open (not a frame).
pub const HEARTBEAT: &str = ": heartbeat\n\n";

/// A conversation or message id as the web writes it: a number, or an empty
/// string on a surface that stores nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireId {
    Number(i64),
    Text(String),
}

impl Default for WireId {
    fn default() -> Self {
        WireId::Text(String::new())
    }
}

impl From<i64> for WireId {
    fn from(v: i64) -> Self {
        WireId::Number(v)
    }
}

/// Why a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoneReason {
    #[default]
    Stop,
    Cancelled,
    Incomplete,
}

/// Severity of a message about the run rather than from the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// Which layer failed, so the client can suggest the right fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorKind {
    /// No usable model is configured, or the named one is disabled.
    Config,
    /// The request itself was malformed or refused before the run.
    Input,
    /// The model provider refused or failed the call.
    Provider,
    /// A tool failed in a way the run could not recover from.
    Tool,
    /// Anything else, including a bug in this module.
    #[default]
    Internal,
}

/// How the turn was billed: per token, or out of a ChatGPT plan (no price).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Billing {
    #[default]
    Metered,
    Subscription,
}

/// One tool call a paused run waits on (the web's `_requirement_payloads`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    /// The requirement's own id; the resume may name it or the call id.
    pub id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    /// The arguments the trader is asked to approve, redacted.
    pub args: Map<String, Value>,
    /// Required arguments the call does not carry; the card refuses to
    /// approve while this is not empty.
    pub missing: Vec<String>,
    /// `confirmation` (the only kind the desktop's tools pause for).
    pub kind: String,
}

/// Running token and cost totals for the turn (each usage frame carries the
/// turn's total so far, not a delta).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    /// Computed locally from the price table; `None` when the model has no
    /// price there, and always `None` for a plan turn.
    pub cost_usd: Option<f64>,
    pub billing: Billing,
    pub model: Option<String>,
    /// From the start of the run to the first visible output.
    pub ttft_ms: Option<f64>,
}

/// Every frame on the wire, discriminated on `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    /// First frame of every run, before any model output.
    Start {
        run_id: String,
        session_id: String,
        conversation_id: WireId,
        /// The stored question that opened the run, for a later edit.
        #[serde(default)]
        user_message_id: WireId,
    },
    /// New characters of the answer; the client appends.
    Token { delta: String },
    /// A tool call was dispatched.
    ToolStart {
        id: String,
        name: String,
        #[serde(default)]
        args: Map<String, Value>,
    },
    /// The matching end of a [`Frame::ToolStart`].
    ToolEnd {
        id: String,
        name: String,
        ok: bool,
        #[serde(default)]
        result: Value,
        #[serde(default)]
        duration: Option<f64>,
    },
    /// New characters of the model's reasoning trace.
    Reasoning { delta: String },
    /// New characters of OpenUI Lang markup from `render_ui`.
    Ui { delta: String },
    /// A chart a tool built from a service call (`kind` picks the renderer).
    Viz {
        #[serde(default)]
        kind: String,
        #[serde(default)]
        spec: Map<String, Value>,
        #[serde(default)]
        title: String,
        #[serde(default)]
        source: String,
    },
    /// Commands for the `/trading` chart, each with an `op`.
    ChartCommand {
        #[serde(default)]
        commands: Vec<Value>,
    },
    /// The run paused for approval. Ends the stream; no done follows.
    Confirm {
        run_id: String,
        session_id: String,
        #[serde(default)]
        requirements: Vec<Requirement>,
    },
    /// A message about the run, shown beside the conversation.
    Notice { level: NoticeLevel, message: String },
    /// The turn's running usage.
    Usage(Usage),
    /// The run failed; only a done frame follows.
    Error {
        message: String,
        #[serde(default)]
        kind: ErrorKind,
    },
    /// Last frame of a run that reached an end.
    Done {
        #[serde(default)]
        reason: DoneReason,
    },
}

impl Frame {
    /// The frame's `type` value.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Frame::Start { .. } => "start",
            Frame::Token { .. } => "token",
            Frame::ToolStart { .. } => "tool_start",
            Frame::ToolEnd { .. } => "tool_end",
            Frame::Reasoning { .. } => "reasoning",
            Frame::Ui { .. } => "ui",
            Frame::Viz { .. } => "viz",
            Frame::ChartCommand { .. } => "chart_command",
            Frame::Confirm { .. } => "confirm",
            Frame::Notice { .. } => "notice",
            Frame::Usage(_) => "usage",
            Frame::Error { .. } => "error",
            Frame::Done { .. } => "done",
        }
    }

    /// The frame as the JSON object the client receives.
    pub fn to_value(&self) -> Value {
        // Serialising an enum of plain data into a `Value` cannot fail.
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// One frame as an SSE data event: `data: {json}\n\n`.
pub fn sse(frame: &Frame) -> String {
    let body = serde_json::to_string(frame).unwrap_or_else(|_| "{}".into());
    format!("data: {}\n\n", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/agent/frames.json"
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Every sample the web wrote reads back into a desktop frame and is
    /// written again byte for byte: same keys, same order, same escaping.
    #[test]
    fn every_web_sample_is_written_byte_for_byte() {
        let f = fixture();
        let samples = f["samples"].as_array().unwrap();
        assert!(samples.len() >= 20);
        for s in samples {
            let frame: Frame = serde_json::from_value(s["dict"].clone())
                .unwrap_or_else(|e| panic!("{}: {}", s["dict"], e));
            assert_eq!(sse(&frame), s["sse"].as_str().unwrap(), "{}", s["dict"]);
        }
    }

    /// The vocabulary and each frame's fields, in the web's order.
    #[test]
    fn frame_types_and_fields_match_the_web() {
        let f = fixture();
        let types: Vec<&str> = f["frame_types"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let ours = [
            Frame::Start {
                run_id: String::new(),
                session_id: String::new(),
                conversation_id: WireId::Number(1),
                user_message_id: WireId::default(),
            },
            Frame::Token { delta: "a".into() },
            Frame::ToolStart {
                id: "i".into(),
                name: "n".into(),
                args: Map::new(),
            },
            Frame::ToolEnd {
                id: "i".into(),
                name: "n".into(),
                ok: true,
                result: Value::Null,
                duration: None,
            },
            Frame::Reasoning { delta: "a".into() },
            Frame::Ui { delta: "a".into() },
            Frame::Viz {
                kind: String::new(),
                spec: Map::new(),
                title: String::new(),
                source: String::new(),
            },
            Frame::ChartCommand { commands: vec![] },
            Frame::Confirm {
                run_id: String::new(),
                session_id: String::new(),
                requirements: vec![],
            },
            Frame::Notice {
                level: NoticeLevel::Info,
                message: String::new(),
            },
            Frame::Usage(Usage::default()),
            Frame::Error {
                message: String::new(),
                kind: ErrorKind::Internal,
            },
            Frame::Done {
                reason: DoneReason::Stop,
            },
        ];
        assert_eq!(ours.iter().map(Frame::wire_type).collect::<Vec<_>>(), types);
        for frame in &ours {
            let v = frame.to_value();
            let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
            let want: Vec<&str> = std::iter::once("type")
                .chain(
                    f["fields"][frame.wire_type()]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x["name"].as_str().unwrap()),
                )
                .collect();
            assert_eq!(keys, want, "{}", frame.wire_type());
        }
    }

    /// A frame written with only its required fields carries the web's
    /// defaults for the rest.
    #[test]
    fn defaults_match_the_web() {
        let f = fixture();
        for (ty, fields) in f["fields"].as_object().unwrap() {
            let mut obj = Map::new();
            obj.insert("type".into(), Value::String(ty.clone()));
            let mut defaults = Map::new();
            for field in fields.as_array().unwrap() {
                let name = field["name"].as_str().unwrap();
                if field["default"] == serde_json::json!({"required": true}) {
                    let filler = match (ty.as_str(), name) {
                        ("tool_end", "ok") => Value::Bool(true),
                        ("notice", "level") => Value::String("info".into()),
                        ("start", "conversation_id") => Value::from(3),
                        _ => Value::String("x".into()),
                    };
                    obj.insert(name.into(), filler);
                } else {
                    defaults.insert(name.into(), field["default"].clone());
                }
            }
            let frame: Frame = serde_json::from_value(Value::Object(obj)).unwrap();
            let written = frame.to_value();
            for (name, want) in defaults {
                assert_eq!(written[&name], want, "{}.{}", ty, name);
            }
        }
    }

    #[test]
    fn enums_headers_and_heartbeat_match_the_web() {
        let f = fixture();
        let names = |key: &str| -> Vec<String> {
            f[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        let as_wire = |v: Value| v.as_str().unwrap().to_string();
        assert_eq!(
            names("done_reasons"),
            [
                DoneReason::Stop,
                DoneReason::Cancelled,
                DoneReason::Incomplete
            ]
            .map(|r| as_wire(serde_json::to_value(r).unwrap()))
        );
        assert_eq!(
            names("notice_levels"),
            [NoticeLevel::Info, NoticeLevel::Warning, NoticeLevel::Error]
                .map(|r| as_wire(serde_json::to_value(r).unwrap()))
        );
        assert_eq!(
            names("error_kinds"),
            [
                ErrorKind::Config,
                ErrorKind::Input,
                ErrorKind::Provider,
                ErrorKind::Tool,
                ErrorKind::Internal
            ]
            .map(|r| as_wire(serde_json::to_value(r).unwrap()))
        );
        let headers: Vec<(String, String)> = f["sse_headers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        assert_eq!(
            headers,
            SSE_HEADERS
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>()
        );
        assert_eq!(f["heartbeat"], HEARTBEAT);
    }
}
