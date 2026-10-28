//! A scripted MCP server behind the transport seam and a clock moved by
//! hand (the web's `FakeServer` and `FakeClock` in `test_agent_mcp.py`).
//! Production registers no server, so tests register these stand-ins.

use super::registry::McpServerSpec;
use super::transport::{Clock, Failure, Timeouts, Transport};
use super::wire::Reply;
use super::{Connector, ExternalData};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const RUPEE: &str = "\u{20b9}";

/// Words a trader must never be shown.
pub const TECHNICAL_WORDS: [&str; 6] = [
    "HTTP",
    "504",
    "JSON-RPC",
    "ReadTimeout",
    "ConnectError",
    "session id",
];

pub fn assert_trader_safe(message: &str) {
    for w in TECHNICAL_WORDS {
        assert!(
            !message.contains(w),
            "{:?} reached a trader-facing message: {:?}",
            w,
            message
        );
    }
}

/// The web's three tools: one with arguments, one without, one denied.
pub fn tools() -> Vec<Value> {
    vec![
        json!({"name": "get_stock_history", "description": format!("Daily OHLCV in {} for one NSE stock.", RUPEE),
               "inputSchema": {"type": "object", "$schema": "http://json-schema.org/draft-07/schema#",
                               "properties": {"symbol": {"type": "string", "description": "NSE symbol"},
                                              "months": {"type": "number", "default": 1}},
                               "required": ["symbol"]}}),
        json!({"name": "get_market_mood", "description": "India VIX mood.", "inputSchema": {"type": "object", "properties": {}}}),
        json!({"name": "admin_purge", "description": "Not for the agent.", "inputSchema": {"type": "object", "properties": {}}}),
    ]
}

/// A clock a test moves by hand.
#[derive(Default)]
pub struct FakeClock(AtomicU64);

impl FakeClock {
    pub fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        Duration::from_millis(1_000_000 + self.0.load(Ordering::SeqCst))
    }
}

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub url: String,
    pub headers: HashMap<String, String>,
    pub payload: Value,
    pub timeouts: Timeouts,
}

impl Seen {
    pub fn method(&self) -> &str {
        self.payload["method"].as_str().unwrap_or("")
    }
}

pub type Handler = Arc<dyn Fn(&FakeServer, &Seen) -> Result<Reply, Failure> + Send + Sync>;

/// A handler from a closure (the bound gives the closure its signature).
pub fn respond<F>(f: F) -> Handler
where
    F: Fn(&FakeServer, &Seen) -> Result<Reply, Failure> + Send + Sync + 'static,
{
    Arc::new(f)
}

/// A healthy server with sessions `s1`, `s2`...; any method's answer can
/// be replaced by a handler.
pub struct FakeServer {
    pub seen: Mutex<Vec<Seen>>,
    pub tools: Mutex<Vec<Value>>,
    pub call_result: Mutex<Value>,
    pub sse: AtomicBool,
    pub sessions: AtomicU64,
    pub live: Mutex<HashSet<String>>,
    pub handlers: Mutex<HashMap<String, Handler>>,
    /// Moved by this much on every request (a slow server).
    pub clock: Option<(Arc<FakeClock>, Duration)>,
}

impl Default for FakeServer {
    fn default() -> Self {
        FakeServer {
            seen: Mutex::default(),
            tools: Mutex::new(tools()),
            call_result: Mutex::new(
                json!({"content": [{"type": "text", "text": "{\"symbol\":\"SBIN\",\"close\":812.5}"}]}),
            ),
            sse: AtomicBool::new(false),
            sessions: AtomicU64::new(0),
            live: Mutex::default(),
            handlers: Mutex::default(),
            clock: None,
        }
    }
}

pub fn reply_raw(status: u16, headers: &[(&str, &str)], text: &str) -> Reply {
    Reply {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        text: text.into(),
        oversized: false,
    }
}

impl FakeServer {
    pub fn on(&self, method: &str, handler: Handler) {
        self.handlers.lock().insert(method.into(), handler);
    }

    pub fn methods(&self) -> Vec<String> {
        self.seen
            .lock()
            .iter()
            .map(|s| s.method().to_string())
            .collect()
    }

    pub fn count(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| *m == method).count()
    }

    /// A JSON-RPC result, as JSON or as an event stream behind a comment
    /// and a progress notification.
    pub fn reply(&self, id: &Value, result: Value, extra: &[(&str, &str)]) -> Reply {
        let message = json!({"jsonrpc": "2.0", "id": id, "result": result});
        let (text, kind) = if self.sse.load(Ordering::SeqCst) {
            (
                format!(": keepalive\n\nevent: message\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}}\n\nevent: message\ndata: {}\n\n", message),
                "text/event-stream",
            )
        } else {
            (message.to_string(), "application/json")
        };
        let mut headers = vec![("content-type", kind)];
        headers.extend_from_slice(extra);
        reply_raw(200, &headers, &text)
    }

    pub fn default_reply(&self, seen: &Seen) -> Result<Reply, Failure> {
        let id = &seen.payload["id"];
        match seen.method() {
            "initialize" => {
                let session = format!("s{}", self.sessions.fetch_add(1, Ordering::SeqCst) + 1);
                self.live.lock().insert(session.clone());
                Ok(self.reply(
                    id,
                    json!({"protocolVersion": "2025-06-18", "serverInfo": {"name": "fake-server", "version": "1"}}),
                    &[("mcp-session-id", &session)],
                ))
            }
            "notifications/initialized" => Ok(reply_raw(202, &[], "")),
            method => {
                let session = seen
                    .headers
                    .get("Mcp-Session-Id")
                    .cloned()
                    .unwrap_or_default();
                if !self.live.lock().contains(&session) {
                    return Ok(reply_raw(
                        404,
                        &[("content-type", "text/plain")],
                        "Session not found",
                    ));
                }
                match method {
                    "tools/list" => Ok(self.reply(id, json!({"tools": *self.tools.lock()}), &[])),
                    "tools/call" => Ok(self.reply(id, self.call_result.lock().clone(), &[])),
                    _ => Ok(reply_raw(400, &[], "unknown method")),
                }
            }
        }
    }
}

/// The transport seam over a shared fake server.
pub struct Seam(pub Arc<FakeServer>);

#[async_trait]
impl Transport for Seam {
    async fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: Vec<u8>,
        timeouts: Timeouts,
    ) -> Result<Reply, Failure> {
        let seen = Seen {
            url: url.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            payload: serde_json::from_slice(&body).unwrap_or(Value::Null),
            timeouts,
        };
        self.0.seen.lock().push(seen.clone());
        if let Some((clock, by)) = &self.0.clock {
            clock.advance(*by);
        }
        let handler = self.0.handlers.lock().get(seen.method()).cloned();
        match handler {
            Some(h) => h(&self.0, &seen),
            None => self.0.default_reply(&seen),
        }
    }
}

/// A test spec with the web's defaults, at an invalid https address.
pub fn spec(key: &str) -> McpServerSpec {
    let mut s = McpServerSpec::new(
        key,
        "Demo end-of-day data",
        &format!("https://example.invalid/{}/mcp", key),
        "Stand-in server for tests.",
        "daily history and breadth in tests",
    );
    s.provider = "Example Data".into();
    s.deny_tools = vec!["admin_purge".into()];
    s
}

/// The web's two stand-in servers.
pub fn demo_servers() -> Vec<McpServerSpec> {
    let mut live = spec("demo_live");
    live.title = "Demo live snapshot".into();
    live.use_for = "live index values in tests".into();
    vec![spec("demo_eod"), live]
}

/// Register `servers` on `hub`, every one served by `server` on `clock`.
pub fn install(
    hub: &ExternalData,
    servers: Vec<McpServerSpec>,
    server: &Arc<FakeServer>,
    clock: &Arc<FakeClock>,
) {
    let s = server.clone();
    let connector: Connector =
        Arc::new(
            move |_spec: &McpServerSpec| Some(Arc::new(Seam(s.clone())) as Arc<dyn Transport>),
        );
    hub.install(servers, connector, clock.clone());
}
