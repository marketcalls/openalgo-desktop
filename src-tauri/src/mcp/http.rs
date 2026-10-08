//! `POST /mcp` and `GET /mcp`: the streamable HTTP transport, at parity with
//! the web's `blueprints/mcp_http.py`.
//!
//! * Bearer token from the API key page; 401 with a `WWW-Authenticate:
//!   Bearer` challenge when it is missing or not live, 403
//!   `insufficient_scope` (same challenge) when the kill switch has
//!   withdrawn the write scope.
//! * JSON-RPC 2.0: `initialize`, `ping`, `tools/list` (filtered by the
//!   token's scopes), `tools/call`; notifications are accepted with 202.
//! * Per-token sliding windows: 60 calls a minute on each read scope, 5 on
//!   `write:orders`, 120 requests a minute in all, 5 event streams a minute.
//! * Every `tools/call` that names a known tool is audited (token, tool,
//!   scope, SHA-256 of the arguments, duration, outcome, address).
//! * `GET /mcp` is a keepalive-only event stream: at most
//!   [`MAX_STREAMS`] at once, each ending after its lifetime (the client is
//!   told when to reconnect) or when the app shuts down, and released when
//!   the client goes away.
//! * Requests from other machines are served only when Remote MCP is on in
//!   the admin page; loopback clients always are.

use super::store::{self, AuditEntry, TokenRow};
use super::{schema, tools, Scope};
use crate::server::middleware::ClientIp;
use crate::state::AppState;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

type Ctx = State<Arc<AppState>>;

/// MCP protocol version the HTTP transport speaks (web `initialize`).
pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub const READ_LIMIT: (usize, Duration, &str) = (60, Duration::from_secs(60), "60 per minute");
pub const WRITE_LIMIT: (usize, Duration, &str) = (5, Duration::from_secs(60), "5 per minute");
pub const DISPATCH_LIMIT: (usize, Duration) = (120, Duration::from_secs(60));
pub const SSE_LIMIT: (usize, Duration) = (5, Duration::from_secs(60));

/// Event streams open at once (web `MCP_SSE_MAX_STREAMS`).
pub const MAX_STREAMS: usize = 4;
/// A stream's lifetime (web `MCP_SSE_MAX_SECONDS`).
pub const STREAM_LIFETIME: Duration = Duration::from_secs(300);
/// Keepalive cadence (web `_SSE_KEEPALIVE_SECONDS`).
pub const KEEPALIVE: Duration = Duration::from_secs(15);
/// Reconnect delay advertised on a stream (web `MCP_SSE_RETRY_MS`).
pub const RETRY_MS: u64 = 30_000;

pub const SSE_BUSY_MESSAGE: &str =
    "OpenAlgo is already holding as many MCP event streams as it allows. Try again in a moment.";
pub const WRITES_DISABLED_MESSAGE: &str =
    "Order tools are turned off for AI clients (the MCP kill switch is on). \
Turn the write scope back on under Admin, Remote MCP to allow them again.";

/// Sweep idle rate-limit buckets this often.
const SWEEP_EVERY: Duration = Duration::from_secs(300);

/// Per-process MCP state: rate-limit windows and the stream count. Bounded:
/// buckets idle longer than the longest window are swept.
pub struct McpRuntime {
    buckets: Mutex<(HashMap<String, VecDeque<Instant>>, Instant)>,
    streams: Arc<AtomicUsize>,
    lifetime: Mutex<Duration>,
    keepalive: Mutex<Duration>,
    research: Arc<tokio::sync::Semaphore>,
    inflight: Arc<Mutex<HashMap<String, usize>>>,
}

impl Default for McpRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl McpRuntime {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new((HashMap::new(), Instant::now())),
            streams: Arc::new(AtomicUsize::new(0)),
            lifetime: Mutex::new(STREAM_LIFETIME),
            keepalive: Mutex::new(KEEPALIVE),
            research: Arc::new(tokio::sync::Semaphore::new(super::research::RESEARCH_SLOTS)),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Slots for research calls running at once (process-wide).
    pub fn research_slots(&self) -> Arc<tokio::sync::Semaphore> {
        self.research.clone()
    }

    /// Event streams open now.
    pub fn open_streams(&self) -> usize {
        self.streams.load(Ordering::SeqCst)
    }

    /// Rate-limit buckets held (bounded by the tokens active in a window).
    pub fn bucket_count(&self) -> usize {
        self.buckets.lock().0.len()
    }

    /// Shorten the stream timings (tests).
    pub fn set_stream_timing(&self, lifetime: Duration, keepalive: Duration) {
        *self.lifetime.lock() = lifetime;
        *self.keepalive.lock() = keepalive;
    }

    /// Count one hit in `key`'s window unless it is full. The prune, the
    /// test and the append happen under one lock.
    pub fn hit(&self, key: &str, limit: usize, window: Duration) -> bool {
        self.reserve(key, 1, limit, window)
    }

    /// Count `n` hits at once, all or none: refused when they would not all
    /// fit in the window (a call's whole fan-out is paid before it starts).
    pub fn reserve(&self, key: &str, n: usize, limit: usize, window: Duration) -> bool {
        let now = Instant::now();
        let mut g = self.buckets.lock();
        let (map, last_sweep) = &mut *g;
        if now.duration_since(*last_sweep) >= SWEEP_EVERY {
            *last_sweep = now;
            let horizon = Duration::from_secs(60);
            map.retain(|_, b| b.back().is_some_and(|t| now.duration_since(*t) < horizon));
        }
        let b = map.entry(key.to_string()).or_default();
        while b.front().is_some_and(|t| now.duration_since(*t) >= window) {
            b.pop_front();
        }
        if b.len() + n > limit {
            return false;
        }
        b.extend(std::iter::repeat_n(now, n));
        true
    }

    /// Hits left in `key`'s window (tests and diagnostics).
    pub fn remaining(&self, key: &str, limit: usize, window: Duration) -> usize {
        let now = Instant::now();
        let g = self.buckets.lock();
        let used =
            g.0.get(key)
                .map(|b| {
                    b.iter()
                        .filter(|t| now.duration_since(**t) < window)
                        .count()
                })
                .unwrap_or(0);
        limit.saturating_sub(used)
    }

    /// Take one of `key`'s `max` in-flight slots; released when the guard
    /// drops. The map only holds keys with calls running.
    pub fn enter(&self, key: &str, max: usize) -> Option<InflightGuard> {
        let mut m = self.inflight.lock();
        let n = m.entry(key.to_string()).or_insert(0);
        if *n >= max {
            if *n == 0 {
                m.remove(key);
            }
            return None;
        }
        *n += 1;
        Some(InflightGuard {
            map: self.inflight.clone(),
            key: key.to_string(),
        })
    }

    /// Calls of `key` running now.
    pub fn in_flight(&self, key: &str) -> usize {
        self.inflight.lock().get(key).copied().unwrap_or(0)
    }

    fn admit_stream(&self) -> Option<StreamGuard> {
        let mut cur = self.streams.load(Ordering::SeqCst);
        loop {
            if cur >= MAX_STREAMS {
                return None;
            }
            match self
                .streams
                .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Some(StreamGuard(self.streams.clone())),
                Err(now) => cur = now,
            }
        }
    }
}

/// Releases an in-flight slot (see [`McpRuntime::enter`]).
pub struct InflightGuard {
    map: Arc<Mutex<HashMap<String, usize>>>,
    key: String,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut m = self.map.lock();
        if let Some(n) = m.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

/// Releases a stream slot when the stream ends, however it ends.
struct StreamGuard(Arc<AtomicUsize>);

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// ----------------------------------------------------------------------
// Responses
// ----------------------------------------------------------------------

/// RFC 6750 challenge (web `_unauthorized`): 401 for `invalid_token`, 403
/// otherwise.
pub fn unauthorized(ctx: &AppState, code: &str, description: &str) -> Response {
    let base = public_url(ctx);
    let mut challenge = format!("Bearer realm=\"openalgo-mcp\", error=\"{}\"", code);
    if !description.is_empty() {
        challenge.push_str(&format!(", error_description=\"{}\"", description));
    }
    challenge.push_str(&format!(
        ", resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
        base
    ));
    let status = if code == "invalid_token" {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::FORBIDDEN
    };
    let mut resp = (
        status,
        Json(json!({"error": code, "error_description": description})),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&challenge) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    resp
}

fn public_url(ctx: &AppState) -> String {
    ctx.sqlite
        .conn()
        .ok()
        .and_then(|c| store::settings(&c).ok())
        .map(|s| s.public_url.trim_end_matches('/').to_string())
        .unwrap_or_default()
}

fn rpc_error(id: &Value, code: i64, message: &str, data: Option<Value>) -> Response {
    let mut err = json!({"code": code, "message": message});
    if let Some(d) = data {
        err["data"] = d;
    }
    Json(json!({"jsonrpc": "2.0", "id": id, "error": err})).into_response()
}

fn rpc_result(id: &Value, result: Value) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn too_many() -> Response {
    let mut r = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": "rate_limited",
            "error_description": "Too many requests from this AI client. Wait a minute and try again.",
        })),
    )
        .into_response();
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    r
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"status": "error", "message": "Not found", "path": "/mcp"})),
    )
        .into_response()
}

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn bearer(headers: &HeaderMap) -> Option<String> {
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    if auth.len() < 7 || !auth[..7].eq_ignore_ascii_case("bearer ") {
        return None;
    }
    let t = auth[7..].trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_loopback(),
        IpAddr::V6(v) => v.is_loopback() || v.to_ipv4_mapped().is_some_and(|m| m.is_loopback()),
    }
}

/// Remote MCP off: a request from another machine finds no `/mcp`.
fn reachable(ctx: &AppState, ip: IpAddr) -> bool {
    is_local(ip)
        || ctx
            .sqlite
            .conn()
            .ok()
            .and_then(|c| store::settings(&c).ok())
            .is_some_and(|s| s.http_enabled)
}

/// DNS-rebinding and cross-site guard: a request that carries an `Origin`
/// must come from the app's own origin (loopback, or the LAN address when
/// the trader bound beyond loopback), or from the Remote MCP public URL
/// when Remote MCP is on. Clients that are not browsers send no `Origin`.
/// (The `Host` header is checked for every route by `host_check`.)
pub fn origin_allowed(ctx: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    if origin == "null" {
        return false;
    }
    let settings = ctx
        .sqlite
        .conn()
        .ok()
        .and_then(|c| store::settings(&c).ok());
    if let Some(s) = &settings {
        let public = s.public_url.trim_end_matches('/');
        if s.http_enabled && !public.is_empty() && origin.eq_ignore_ascii_case(public) {
            return true;
        }
    }
    match origin.strip_prefix("http://") {
        Some(host) => crate::server::middleware::host_allowed(ctx, host),
        None => false,
    }
}

fn bad_origin() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": "invalid_origin", "error_description": "Request blocked."})),
    )
        .into_response()
}

/// The live token in the request, if any.
#[allow(clippy::result_large_err)]
fn token(ctx: &AppState, headers: &HeaderMap) -> Result<TokenRow, Response> {
    let Some(t) = bearer(headers) else {
        return Err(unauthorized(ctx, "invalid_token", "Missing Bearer token."));
    };
    let found = ctx.sqlite.conn().and_then(|c| store::find_token(&c, &t));
    match found {
        Ok(Some(row)) => {
            if let Ok(c) = ctx.sqlite.conn() {
                let _ = store::touch_token(&c, row.id, ctx.now());
            }
            Ok(row)
        }
        Ok(None) => Err(unauthorized(ctx, "invalid_token", "")),
        Err(e) => {
            tracing::error!("MCP token check failed: {}", e);
            Err(unauthorized(ctx, "invalid_token", ""))
        }
    }
}

/// Python `json.dumps(v, sort_keys=True)` (default separators, ASCII).
pub fn py_dumps(v: &Value) -> String {
    fn esc(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{08}' => out.push_str("\\b"),
                '\u{0c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                    let mut buf = [0u16; 2];
                    for u in c.encode_utf16(&mut buf) {
                        out.push_str(&format!("\\u{:04x}", u));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
    fn go(v: &Value, out: &mut String) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => esc(s, out),
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    go(x, out);
                }
                out.push(']');
            }
            Value::Object(o) => {
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    esc(k, out);
                    out.push_str(": ");
                    go(&o[k.as_str()], out);
                }
                out.push('}');
            }
        }
    }
    let mut s = String::new();
    go(v, &mut s);
    s
}

/// Web `_params_hash`: first 16 hex of SHA-256 over sorted-key JSON.
pub fn params_hash(args: &Value) -> String {
    hex::encode(Sha256::digest(py_dumps(args).as_bytes()))[..16].to_string()
}

struct Call<'a> {
    ctx: &'a AppState,
    token: &'a TokenRow,
    tool: &'a str,
    scope: &'a str,
    hash: String,
    ip: IpAddr,
}

impl Call<'_> {
    fn audit(&self, outcome: &str, duration_ms: i64) {
        let entry = AuditEntry {
            ts: self.ctx.now().format("%Y-%m-%d %H:%M:%S").to_string(),
            jti: Some(self.token.jti()),
            client_id: self.token.name.clone(),
            tool: self.tool.to_string(),
            scope: self.scope.to_string(),
            params_hash: self.hash.clone(),
            duration_ms,
            outcome: outcome.to_string(),
            request_ip: self.ip.to_string(),
        };
        let r = self.ctx.logs.conn().and_then(|c| store::audit(&c, &entry));
        if let Err(e) = r {
            tracing::error!("MCP audit entry not written: {}", e);
        }
    }
}

// ----------------------------------------------------------------------
// Handlers
// ----------------------------------------------------------------------

/// POST /mcp
pub async fn post(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !reachable(&ctx, ip) {
        return not_found();
    }
    if !origin_allowed(&ctx, &headers) {
        return bad_origin();
    }
    let tok = match token(&ctx, &headers) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let (n, w) = DISPATCH_LIMIT;
    if !ctx.mcp.hit(&format!("{}|dispatch", tok.jti()), n, w) {
        return too_many();
    }
    let Ok(Value::Object(msg)) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(
            &Value::Null,
            -32700,
            "Parse error: body must be a JSON object.",
            None,
        );
    };
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return rpc_error(&id, -32600, "Invalid Request: jsonrpc must be 2.0.", None);
    }
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !msg.contains_key("id") && method.starts_with("notifications/") {
        return StatusCode::ACCEPTED.into_response();
    }
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let scopes = tok.scope.grants();
    match method {
        "initialize" => rpc_result(
            &id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": {"name": "openalgo", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"tools": {"listChanged": false}},
            }),
        ),
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => {
            let list: Vec<Value> = super::tools_for_scopes(&scopes)
                .iter()
                .map(|t| t.descriptor())
                .collect();
            rpc_result(&id, json!({"tools": list}))
        }
        "tools/call" => call_tool(&ctx, &tok, &scopes, ip, &id, params).await,
        other => rpc_error(&id, -32601, &format!("Method not found: {}", other), None),
    }
}

async fn call_tool(
    ctx: &Arc<AppState>,
    tok: &TokenRow,
    scopes: &[Scope],
    ip: IpAddr,
    id: &Value,
    params: Value,
) -> Response {
    let params = match params {
        Value::Null => Map::new(),
        Value::Object(m) => m,
        _ => return rpc_error(id, -32602, "Invalid params: must be an object.", None),
    };
    let Some(name) = params
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
    else {
        return rpc_error(id, -32602, "Invalid params: 'name' is required.", None);
    };
    let args = match params.get("arguments") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => {
            return rpc_error(
                id,
                -32602,
                "Invalid params: 'arguments' must be an object.",
                None,
            )
        }
    };
    let Some(tool) = super::tool(name) else {
        return rpc_error(id, -32601, &format!("Unknown tool: {}", name), None);
    };
    let needed = tool.scope.as_str();
    let call = Call {
        ctx,
        token: tok,
        tool: tool.name,
        scope: needed,
        hash: params_hash(&Value::Object(args.clone())),
        ip,
    };
    if !scopes.contains(&tool.scope) {
        call.audit("insufficient_scope", 0);
        return rpc_error(
            id,
            -32000,
            "insufficient_scope",
            Some(json!({"required_scope": needed})),
        );
    }
    if tool.scope == Scope::WriteOrders {
        let writes_on = ctx
            .sqlite
            .conn()
            .and_then(|c| store::settings(&c))
            .map(|s| s.write_scope_enabled)
            .unwrap_or(false);
        if !writes_on {
            call.audit("writes_disabled", 0);
            return unauthorized(ctx, "insufficient_scope", WRITES_DISABLED_MESSAGE);
        }
    }
    let (limit, window, spec) = if tool.scope == Scope::WriteOrders {
        WRITE_LIMIT
    } else {
        READ_LIMIT
    };
    if !ctx
        .mcp
        .hit(&format!("{}|{}", tok.jti(), needed), limit, window)
    {
        call.audit("rate_limited", 0);
        return rpc_error(
            id,
            -32000,
            "rate_limited",
            Some(json!({"scope": needed, "limit": spec})),
        );
    }
    let bound = match schema::bind(tool.params, &args) {
        Ok(b) => b,
        Err(e) => {
            tracing::info!("MCP {} refused: {}", tool.name, e.0);
            call.audit("bad_arguments", 0);
            return rpc_error(
                id,
                -32603,
                "tool_error",
                Some(json!({"reason": "Invalid arguments. Check the tool schema."})),
            );
        }
    };
    if tool.scope == Scope::WriteOrders {
        // Web `_notify_pre_write`: surfaced before the order goes out
        // (arguments stay in the audit hash, never in the log).
        tracing::warn!("MCP write tool {} called by '{}'", tool.name, tok.name);
    }
    let started = Instant::now();
    // The tool's own /api/v1 calls are charged to this token.
    let budget = Arc::new(super::dispatch::CallBudget::new(tok.jti()));
    let text = super::dispatch::CALLER
        .scope(budget, tools::call(ctx, tool, &bound))
        .await;
    call.audit("success", started.elapsed().as_millis() as i64);
    rpc_result(
        id,
        json!({"content": [{"type": "text", "text": text}], "isError": false}),
    )
}

/// GET /mcp: keepalive-only event stream.
pub async fn sse(State(ctx): Ctx, ClientIp(ip): ClientIp, headers: HeaderMap) -> Response {
    if !reachable(&ctx, ip) {
        return not_found();
    }
    if !origin_allowed(&ctx, &headers) {
        return bad_origin();
    }
    let tok = match token(&ctx, &headers) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let (n, w) = SSE_LIMIT;
    if !ctx.mcp.hit(&format!("{}|sse", tok.jti()), n, w) {
        return too_many();
    }
    let Some(guard) = ctx.mcp.admit_stream() else {
        let mut r = (StatusCode::TOO_MANY_REQUESTS, SSE_BUSY_MESSAGE).into_response();
        r.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(RETRY_MS / 1000));
        return r;
    };
    let lifetime = *ctx.mcp.lifetime.lock();
    let keepalive = *ctx.mcp.keepalive.lock();
    struct S {
        _guard: StreamGuard,
        ctx: Arc<AppState>,
        token_id: i64,
        first: bool,
        deadline: tokio::time::Instant,
        keepalive: Duration,
        stop: tokio_util::sync::CancellationToken,
    }
    let state = S {
        _guard: guard,
        ctx: ctx.clone(),
        token_id: tok.id,
        first: true,
        deadline: tokio::time::Instant::now() + lifetime,
        keepalive,
        stop: ctx.shutdown.child_token(),
    };
    let stream = futures_util::stream::unfold(state, |mut s| async move {
        if s.first {
            s.first = false;
            let hello = format!("retry: {}\n: openalgo-mcp connected\n\n", RETRY_MS);
            return Some((Ok::<Bytes, std::convert::Infallible>(Bytes::from(hello)), s));
        }
        let now = tokio::time::Instant::now();
        if now >= s.deadline {
            return None;
        }
        let wait = s.keepalive.min(s.deadline - now);
        tokio::select! {
            _ = s.stop.cancelled() => None,
            _ = tokio::time::sleep(wait) => {
                // A token revoked while its stream is open ends the stream.
                let live = s
                    .ctx
                    .sqlite
                    .conn()
                    .and_then(|c| store::token_is_live(&c, s.token_id))
                    .unwrap_or(false);
                if !live || tokio::time::Instant::now() >= s.deadline {
                    None
                } else {
                    Some((Ok(Bytes::from_static(b": keepalive\n\n")), s))
                }
            }
        }
    });
    let mut r = Response::new(Body::from_stream(stream));
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    r
}

/// GET /mcp/healthz (no auth).
pub async fn healthz() -> Response {
    Json(json!({"status": "ok", "service": "openalgo-mcp"})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_hash_matches_python() {
        // hashlib.sha256(json.dumps({"symbol": "SBIN", "quantity": 1},
        // sort_keys=True).encode()).hexdigest()[:16]
        assert_eq!(
            py_dumps(&json!({"symbol": "SBIN", "quantity": 1})),
            r#"{"quantity": 1, "symbol": "SBIN"}"#
        );
        assert_eq!(params_hash(&json!({})).len(), 16);
        // ensure_ascii: non-ASCII is escaped as \uXXXX.
        assert_eq!(py_dumps(&json!(["\u{e9}"])), "[\"\\u00e9\"]");
    }

    #[test]
    fn sliding_window_counts_per_key() {
        let rt = McpRuntime::new();
        for _ in 0..5 {
            assert!(rt.hit("a", 5, Duration::from_secs(60)));
        }
        assert!(!rt.hit("a", 5, Duration::from_secs(60)));
        assert!(rt.hit("b", 5, Duration::from_secs(60)));
        assert_eq!(rt.bucket_count(), 2);
    }

    #[test]
    fn stream_slots_are_capped_and_released() {
        let rt = McpRuntime::new();
        let guards: Vec<_> = (0..MAX_STREAMS)
            .map(|_| rt.admit_stream().unwrap())
            .collect();
        assert!(rt.admit_stream().is_none());
        drop(guards);
        assert_eq!(rt.open_streams(), 0);
    }

    #[test]
    fn bearer_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer  oamcp_x "),
        );
        assert_eq!(bearer(&h).as_deref(), Some("oamcp_x"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer"));
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer(&h), None);
    }
}
