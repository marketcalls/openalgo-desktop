//! A client for one MCP server over streamable HTTP (the web's
//! `services/agent/mcp/client.py`).
//!
//! The transport is plain: JSON-RPC 2.0 over HTTP POST, answered with a
//! JSON body or a short `text/event-stream`. Four methods cover everything
//! the Agent needs (`initialize`, `notifications/initialized`,
//! `tools/list`, `tools/call`), so this speaks them directly.
//!
//! **Failure handling.** The gateways this was first built against
//! answered the same request in 0.3 s, with a 504 after 10 s, or not at all
//! for 40 s, from one minute to the next. So every request carries the
//! server's connect and read timeouts and one operation is bounded by
//! `call_deadline` across its retry and any session re-initialisation; a
//! timeout, a failed connection or a 5xx is retried at most `max_retries`
//! times, and only while the deadline has room; a per-server circuit
//! breaker pauses a server after `breaker_threshold` consecutive failed
//! operations, answers "paused" at once for `breaker_cooldown`, then lets
//! one call through to test it; the tool list is cached for
//! `tools_cache_ttl` and a failed fetch remembered for
//! `negative_cache_ttl`.
//!
//! All state sits under one lock held for bookkeeping only; no request is
//! made while it is held. A trial call dropped mid-flight (the trader
//! stopped the turn) frees its slot, so the breaker never sticks.

use super::errors::{self, Kind, McpError};
use super::registry::McpServerSpec;
use super::transport::{Clock, Failure, Timeouts, Transport, MAX_RESPONSE_CHARS};
use super::wire::{content_text, parse_sse, pick_response, Reply};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The protocol revision announced in `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// JSON-RPC error codes that mean a bad tool name or bad arguments, which
/// the model can fix, rather than a broken server.
const ARGUMENT_ERROR_CODES: [i64; 2] = [-32602, -32601];

/// The User-Agent sent on every request: the web's prefix, then the app.
pub fn user_agent() -> String {
    format!(
        "OpenAlgo-Agent/{} OpenAlgo-Desktop",
        env!("CARGO_PKG_VERSION")
    )
}

/// A server's tools as it described them (`name`, `description`,
/// `inputSchema`), after the spec's allowlist and denylist.
pub type ToolList = Arc<Vec<Map<String, Value>>>;

/// One successful `tools/call`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub text: String,
    pub structured: Option<Value>,
    pub elapsed_ms: u64,
}

/// One connection test, as `POST /agent/api/mcp/test` lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerCheck {
    pub key: String,
    pub title: String,
    pub ok: bool,
    pub latency_ms: u64,
    pub tool_count: usize,
    pub message: String,
    pub server_name: String,
}

#[derive(Default)]
struct Session {
    id: Option<String>,
    initialized: bool,
    server_name: String,
}

#[derive(Default)]
struct State {
    failures: u32,
    open_until: Duration,
    probing: bool,
    session: Session,
    tools: Option<(ToolList, Duration)>,
    tools_generation: u64,
    failure: Option<(McpError, Duration)>,
}

/// A failure inside a session: the session expired, or anything else.
enum InSession {
    Expired,
    Failed(McpError),
}

impl From<McpError> for InSession {
    fn from(e: McpError) -> Self {
        InSession::Failed(e)
    }
}

/// One attempt's failure: worth one more attempt, or final.
enum Attempt {
    Retry(McpError),
    Final(McpError),
}

/// The client for one server, shared by every run.
pub struct McpClient {
    pub spec: Arc<McpServerSpec>,
    transport: Arc<dyn Transport>,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    ids: AtomicU64,
}

/// The half-open trial slot, freed if the trial never settles (its future
/// was dropped).
struct Trial<'a> {
    client: &'a McpClient,
    armed: bool,
}

impl Drop for Trial<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.client.state.lock().probing = false;
        }
    }
}

impl McpClient {
    pub fn new(
        spec: Arc<McpServerSpec>,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        McpClient {
            spec,
            transport,
            clock,
            state: Mutex::new(State::default()),
            ids: AtomicU64::new(1),
        }
    }

    /// The server's tools, cached; a failed fetch is remembered briefly and
    /// answered from memory. `refresh` bypasses both.
    pub async fn list_tools(&self, refresh: bool) -> Result<ToolList, McpError> {
        let now = self.clock.now();
        let generation = {
            let state = self.state.lock();
            if !refresh {
                if let Some((tools, until)) = &state.tools {
                    if now < *until {
                        return Ok(tools.clone());
                    }
                }
                if let Some((e, until)) = &state.failure {
                    if now < *until {
                        return Err(e.clone());
                    }
                }
            }
            state.tools_generation
        };
        let listed = self
            .operation("tools/list", json!({}), refresh)
            .await
            .and_then(|result| self.offered_tools(result));
        let mut state = self.state.lock();
        match listed {
            Ok(tools) => {
                let tools: ToolList = Arc::new(tools);
                if state.tools_generation == generation {
                    state.tools =
                        Some((tools.clone(), self.clock.now() + self.spec.tools_cache_ttl));
                    state.tools_generation += 1;
                }
                state.failure = None;
                Ok(tools)
            }
            Err(e) => {
                state.failure = Some((e.clone(), self.clock.now() + self.spec.negative_cache_ttl));
                Err(e)
            }
        }
    }

    /// The tools a `tools/list` result offers through the spec's lists.
    fn offered_tools(&self, result: Value) -> Result<Vec<Map<String, Value>>, McpError> {
        let Some(Value::Array(items)) = result.get("tools").cloned() else {
            return Err(errors::protocol(
                &self.spec,
                "listed its tools in a shape this platform cannot read",
                "",
                0,
            ));
        };
        Ok(items
            .into_iter()
            .filter_map(|t| match t {
                Value::Object(o) => Some(o),
                _ => None,
            })
            .filter(|t| {
                t.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| !n.is_empty() && self.spec.offers(n))
            })
            .collect())
    }

    /// Call one tool, the arguments passed as given: the server is the
    /// authority on them.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<ToolResult, McpError> {
        if !self.spec.offers(name) {
            return Err(errors::tool_error(
                format!("The tool {} is not offered from {}.", name, self.spec.title),
                String::new(),
            ));
        }
        let started = self.clock.now();
        let result = self
            .operation(
                "tools/call",
                json!({"name": name, "arguments": arguments}),
                false,
            )
            .await?;
        let Value::Object(result) = result else {
            return Err(errors::protocol(
                &self.spec,
                "answered a tool call in a shape this platform cannot read",
                "",
                0,
            ));
        };
        let text = content_text(result.get("content"));
        let elapsed_ms = self.clock.now().saturating_sub(started).as_millis() as u64;
        if result.get("isError") == Some(&Value::Bool(true)) {
            return Err(errors::tool_error(
                format!("{} could not answer that request.", self.spec.title),
                text,
            ));
        }
        Ok(ToolResult {
            text,
            structured: result.get("structuredContent").cloned(),
            elapsed_ms,
        })
    }

    /// The connection test: a fresh session and the tool list, ignoring the
    /// breaker and the caches because the trader asked whether the server
    /// answers now. A pass closes the breaker and refreshes the list.
    pub async fn check(&self) -> ServerCheck {
        let started = self.clock.now();
        self.state.lock().session = Session::default();
        let listed = self.list_tools(true).await;
        let latency_ms = self.clock.now().saturating_sub(started).as_millis() as u64;
        let (ok, tool_count, message) = match &listed {
            Ok(tools) => (
                true,
                tools.len(),
                format!(
                    "{} answered and offers {} tools.",
                    self.spec.title,
                    tools.len()
                ),
            ),
            Err(e) => (false, 0, e.message.clone()),
        };
        ServerCheck {
            key: self.spec.key.clone(),
            title: self.spec.title.clone(),
            ok,
            latency_ms,
            tool_count,
            message,
            server_name: if ok {
                self.state.lock().session.server_name.clone()
            } else {
                String::new()
            },
        }
    }

    /// The breaker's state: `(failures, open, retry after)`.
    pub fn breaker(&self) -> (u32, bool, Duration) {
        let now = self.clock.now();
        let state = self.state.lock();
        (
            state.failures,
            state.open_until > now,
            state.open_until.saturating_sub(now),
        )
    }

    /// Whether a half-open trial is in flight.
    #[cfg(test)]
    pub fn probing(&self) -> bool {
        self.state.lock().probing
    }

    /// Whether an operation may go out now; true when it is the half-open
    /// trial. Paused while open or while a trial is in flight.
    fn admit(&self, force: bool) -> Result<bool, McpError> {
        let now = self.clock.now();
        let mut state = self.state.lock();
        if force || state.failures < self.spec.breaker_threshold {
            return Ok(false);
        }
        if now < state.open_until || state.probing {
            let retry_after = state
                .open_until
                .saturating_sub(now)
                .max(Duration::from_secs(1));
            return Err(errors::paused(&self.spec, retry_after));
        }
        state.probing = true;
        Ok(true)
    }

    /// Record an operation on the breaker: an answer resets it, an outage
    /// counts, and enough of them open it for the cooldown.
    fn settle(&self, answered: bool, trial: &mut Trial<'_>) {
        trial.armed = false;
        let now = self.clock.now();
        let mut state = self.state.lock();
        state.probing = false;
        if answered {
            state.failures = 0;
            state.open_until = Duration::ZERO;
            return;
        }
        state.failures += 1;
        if state.failures >= self.spec.breaker_threshold {
            state.open_until = now + self.spec.breaker_cooldown;
            drop(state);
            tracing::warn!(
                "MCP server {} paused for {}s after {} consecutive failures",
                self.spec.key,
                self.spec.breaker_cooldown.as_secs(),
                self.spec.breaker_threshold
            );
        }
    }

    /// One request inside a session, under the breaker and the deadline.
    async fn operation(&self, method: &str, params: Value, force: bool) -> Result<Value, McpError> {
        let is_trial = self.admit(force)?;
        let mut trial = Trial {
            client: self,
            armed: is_trial,
        };
        let started = self.clock.now();
        let deadline = started + self.spec.call_deadline;
        let result = self.in_session(method, &params, deadline).await;
        let elapsed = self.clock.now().saturating_sub(started).as_millis();
        match &result {
            Ok(_) => {
                self.settle(true, &mut trial);
                tracing::info!(
                    "MCP {} {} answered in {} ms",
                    self.spec.key,
                    method,
                    elapsed
                );
            }
            Err(e) => {
                self.settle(!e.is_outage(), &mut trial);
                if e.is_outage() {
                    tracing::warn!(
                        "MCP {} {} failed after {} ms: {}",
                        self.spec.key,
                        method,
                        elapsed,
                        e.detail
                    );
                }
            }
        }
        result
    }

    /// Send a request, initialising first, and once more if the session
    /// expired.
    async fn in_session(
        &self,
        method: &str,
        params: &Value,
        deadline: Duration,
    ) -> Result<Value, McpError> {
        for attempt in 0..2 {
            let session = self.ensure_session(deadline).await?;
            match self.rpc(method, params, session.as_deref(), deadline).await {
                Ok(v) => return Ok(v),
                Err(InSession::Failed(e)) => return Err(e),
                Err(InSession::Expired) => {
                    let mut state = self.state.lock();
                    if state.session.id == session {
                        state.session = Session::default();
                    }
                    drop(state);
                    if attempt == 0 {
                        tracing::info!("MCP {} session expired; initialising again", self.spec.key);
                    }
                }
            }
        }
        Err(errors::protocol(
            &self.spec,
            "kept refusing a fresh session",
            "session expired twice",
            0,
        ))
    }

    /// The live session id, running `initialize` when there is none.
    async fn ensure_session(&self, deadline: Duration) -> Result<Option<String>, McpError> {
        {
            let state = self.state.lock();
            if state.session.initialized {
                return Ok(state.session.id.clone());
            }
        }
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        let ua = user_agent();
        let message = json!({
            "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "openalgo-agent", "version": ua},
            },
        });
        let reply = self.send(&message, None, deadline).await?;
        if reply.status == 404 {
            return Err(errors::protocol(
                &self.spec,
                "refused to start a session",
                "initialize 404",
                404,
            ));
        }
        let response = self.decode(&reply, id)?;
        let result = self.result_of(response, "initialize")?;
        let session = reply
            .header("mcp-session-id")
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let server_name = result
            .get("serverInfo")
            .and_then(|i| i.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // Required before any other request. A server that answers it with
        // an error has still given a session, so its status is not checked.
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        self.send(&note, session.as_deref(), deadline).await?;
        self.state.lock().session = Session {
            id: session.clone(),
            initialized: true,
            server_name,
        };
        Ok(session)
    }

    /// One request inside an established session.
    async fn rpc(
        &self,
        method: &str,
        params: &Value,
        session: Option<&str>,
        deadline: Duration,
    ) -> Result<Value, InSession> {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let reply = self.send(&message, session, deadline).await?;
        let mentions_session = || {
            let head: String = reply.text.chars().take(500).collect();
            head.to_lowercase().contains("session")
        };
        if session.is_some() && (reply.status == 404 || (reply.status == 400 && mentions_session()))
        {
            return Err(InSession::Expired);
        }
        let response = self.decode(&reply, id)?;
        Ok(self.result_of(response, method)?)
    }

    /// POST one message, retrying a timeout, a failed connection or a 5xx
    /// while the deadline has room for another attempt.
    async fn send(
        &self,
        message: &Value,
        session: Option<&str>,
        deadline: Duration,
    ) -> Result<Reply, McpError> {
        let body = serde_json::to_vec(message).unwrap_or_default();
        let mut headers: Vec<(&'static str, String)> = vec![
            ("Content-Type", "application/json".into()),
            ("Accept", "application/json, text/event-stream".into()),
            ("User-Agent", user_agent()),
            ("MCP-Protocol-Version", PROTOCOL_VERSION.into()),
        ];
        if let Some(id) = session {
            headers.push(("Mcp-Session-Id", id.to_string()));
        }
        let mut last: Option<McpError> = None;
        for attempt in 0..=self.spec.max_retries {
            let remaining = deadline.saturating_sub(self.clock.now());
            // A retry that cannot even connect inside the deadline only adds
            // waiting to a failure that is already certain.
            if (attempt > 0 && remaining < self.spec.connect_timeout + Duration::from_secs(1))
                || remaining.is_zero()
            {
                break;
            }
            let timeouts = Timeouts {
                connect: self.spec.connect_timeout.min(remaining),
                read: self.spec.read_timeout.min(remaining),
                total: remaining,
            };
            match self.attempt(body.clone(), &headers, timeouts).await {
                Ok(reply) => return Ok(reply),
                Err(Attempt::Final(e)) => return Err(e),
                Err(Attempt::Retry(e)) => {
                    if attempt < self.spec.max_retries {
                        tracing::info!(
                            "MCP {} attempt {} failed ({}); retrying",
                            self.spec.key,
                            attempt + 1,
                            e.detail
                        );
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| {
            errors::timed_out(&self.spec, "deadline reached before the request was sent")
        }))
    }

    /// One HTTP attempt, every failure translated.
    async fn attempt(
        &self,
        body: Vec<u8>,
        headers: &[(&'static str, String)],
        timeouts: Timeouts,
    ) -> Result<Reply, Attempt> {
        let reply = match self
            .transport
            .post(&self.spec.url, headers, body, timeouts)
            .await
        {
            Ok(reply) => reply,
            Err(Failure::TimedOut(d)) => {
                return Err(Attempt::Retry(errors::timed_out(&self.spec, &d)))
            }
            Err(Failure::Unreachable(d)) => {
                return Err(Attempt::Retry(errors::unreachable(&self.spec, &d)))
            }
        };
        if reply.status >= 500 {
            let detail = format!("HTTP {}", reply.status);
            return Err(Attempt::Retry(errors::unreachable(&self.spec, &detail)));
        }
        if reply.status == 429 {
            return Err(Attempt::Final(errors::slow_down(&self.spec)));
        }
        Ok(reply)
    }

    /// The JSON-RPC response to request `id` in a reply.
    fn decode(&self, reply: &Reply, id: u64) -> Result<Map<String, Value>, McpError> {
        if reply.status >= 400 {
            let head: String = reply.text.chars().take(200).collect();
            let detail = format!("HTTP {}: {:?}", reply.status, head);
            return Err(errors::protocol(
                &self.spec,
                "refused the request",
                &detail,
                reply.status,
            ));
        }
        if reply.oversized || reply.text.chars().count() > MAX_RESPONSE_CHARS {
            return Err(errors::protocol(
                &self.spec,
                "sent a reply too large to use",
                "",
                0,
            ));
        }
        let event_stream = reply
            .header("content-type")
            .is_some_and(|t| t.to_lowercase().contains("text/event-stream"));
        let messages = if event_stream {
            parse_sse(&reply.text)
        } else if reply.text.trim().is_empty() {
            Vec::new()
        } else {
            vec![serde_json::from_str(&reply.text).map_err(|_| {
                errors::protocol(
                    &self.spec,
                    "sent a reply that is not readable data",
                    "bad JSON",
                    0,
                )
            })?]
        };
        pick_response(messages, id).ok_or_else(|| {
            errors::protocol(
                &self.spec,
                "sent no answer to the request",
                "no response",
                0,
            )
        })
    }

    /// A response's `result`, or its `error` as a typed one.
    fn result_of(&self, mut response: Map<String, Value>, method: &str) -> Result<Value, McpError> {
        let Some(error) = response.remove("error").filter(|e| !e.is_null()) else {
            return Ok(response.remove("result").unwrap_or(Value::Null));
        };
        let code = error.get("code").and_then(Value::as_i64);
        let text: String = match (&error, error.get("message")) {
            (_, Some(Value::String(m))) => m.clone(),
            (Value::Object(_), other) => other.map_or("None".into(), |m| m.to_string()),
            (Value::String(s), _) => s.clone(),
            (other, _) => other.to_string(),
        }
        .chars()
        .take(1000)
        .collect();
        if method == "tools/call" && code.is_some_and(|c| ARGUMENT_ERROR_CODES.contains(&c)) {
            return Err(errors::tool_error(
                format!(
                    "{} refused that tool name or its arguments.",
                    self.spec.title
                ),
                text,
            ));
        }
        let head: String = text.chars().take(200).collect();
        let detail = format!("{} JSON-RPC error {:?}: {:?}", method, code, head);
        Err(errors::protocol(
            &self.spec,
            "returned an error",
            &detail,
            0,
        ))
    }
}

/// Whether an error is the server's own refusal of the tool or its
/// arguments, carrying its words.
pub fn tool_text(e: &McpError) -> Option<&str> {
    match &e.kind {
        Kind::Tool(text) => Some(text),
        _ => None,
    }
}
