//! In-process `/api/v1` calls for the tools, with the Python SDK's reply
//! shaping.
//!
//! A tool builds the payload the SDK would send and this module hands it to
//! the `/api/v1` handler stack as a request that never touches a socket (the
//! web's `_InProcessWsgi`). The handlers validate it, check the key, choose
//! sandbox or live, publish events and answer exactly as for an SDK client.
//! The reply is then shaped as the SDK's `_handle_response` shapes it, since
//! that dict is what the web's tools serialise.
//!
//! The call runs as a task owned by the app context, so a slow broker never
//! cancels an order mid-flight: after [`SDK_TIMEOUT`] the tool answers with
//! the SDK's `timeout_error` (which the write tools turn into "verify before
//! retrying") and the call finishes on its own.
//!
//! The requests come from [`MCP_CALLER`], an address in the discard-only
//! prefix `100::/64`, so the per-address API-key failure throttle never
//! counts an MCP call against a real local client (an SDK script on
//! 127.0.0.1) or the other way round.
//!
//! # Bounded fan-out
//!
//! A research tool can turn one MCP call into several `/api/v1` calls (a
//! screen over a watchlist, several timeframes). Every one of them passes:
//!
//! * the `/api/v1` rate limiter (the same `api_rate_limit` layer and buckets
//!   as SDK clients: 100 a second, 10 a second for orders), keyed on
//!   [`MCP_CALLER`], so all MCP traffic together stays inside one client's
//!   budget and therefore inside what the broker adapters are sized for;
//! * a per-token upstream budget of [`UPSTREAM_LIMIT`] calls a minute,
//!   charged to the token the HTTP transport scoped the call to
//!   ([`CALLER`]);
//! * a cap of [`MAX_REPLY_BYTES`] on the reply body read back.

use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request};
use axum::middleware as mw;
use http_body_util::{BodyExt, Limited};
use serde_json::{json, Map, Value};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// The address MCP calls carry (discard-only prefix, RFC 6666).
pub const MCP_CALLER: IpAddr = IpAddr::V6(Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 1));

/// The SDK's request timeout (`BaseAPI(timeout=120.0)`).
pub const SDK_TIMEOUT: Duration = Duration::from_secs(120);

/// `/api/v1` calls one token's tools may make in a minute.
pub const UPSTREAM_LIMIT: (usize, Duration) = (120, Duration::from_secs(60));

/// Largest `/api/v1` reply a tool reads (an exchange's full instrument
/// list is the largest legitimate one, well under this).
pub const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

/// The upstream budget of one tool call: the token it is charged to, and
/// calls already paid for up front by a reservation.
#[derive(Debug)]
pub struct CallBudget {
    pub key: String,
    pub prepaid: std::sync::atomic::AtomicUsize,
}

impl CallBudget {
    pub fn new(token_id: String) -> Self {
        Self {
            key: format!("{}|upstream", token_id),
            prepaid: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Pay for `n` calls before the work starts; false when the token's
    /// window cannot hold them all.
    pub fn reserve(&self, ctx: &AppState, n: usize) -> bool {
        let (limit, window) = UPSTREAM_LIMIT;
        if !ctx.mcp.reserve(&self.key, n, limit, window) {
            return false;
        }
        self.prepaid
            .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
        true
    }

    /// Charge one call: from the reservation, else from the window.
    fn charge(&self, ctx: &AppState) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        if self
            .prepaid
            .fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return true;
        }
        let (limit, window) = UPSTREAM_LIMIT;
        ctx.mcp.hit(&self.key, limit, window)
    }
}

tokio::task_local! {
    /// The budget a tool call runs under, set by the HTTP transport around
    /// the call; its `/api/v1` calls are charged to it.
    pub static CALLER: Arc<CallBudget>;
}

/// What the handler stack answered.
#[derive(Debug, Clone)]
pub struct Raw {
    pub status: u16,
    pub text: String,
    /// The reply is for an order placement whose outcome at the broker is
    /// unknown (LOG-08): it may have been placed.
    pub outcome_unknown: bool,
}

impl Raw {
    pub fn new(status: u16, text: impl Into<String>) -> Self {
        Self {
            status,
            text: text.into(),
            outcome_unknown: false,
        }
    }
}

/// Why no answer arrived (MCP-01: "never sent" and "sent, no answer" are
/// different facts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// No answer within the SDK's timeout; the call may have taken effect.
    Timeout,
    /// The request was never handed to the handlers (it could not be
    /// built): nothing ran, so nothing changed.
    NotSent,
    /// The handlers ran, but no answer came back (the reply could not be
    /// read, the handler failed, or the call was cut off): it may have
    /// taken effect.
    NoReply,
    /// The token's upstream budget for this minute is spent.
    Budget,
}

/// The stored API key (the web's `get_first_available_api_key`), or the
/// web's placeholder when none exists, which the handlers refuse as an
/// invalid key.
pub fn api_key(ctx: &AppState) -> String {
    match ApiKeyService::current(ctx) {
        Ok(Some(k)) => k.expose().to_string(),
        Ok(None) => "<not-configured>".into(),
        Err(e) => {
            tracing::warn!("MCP could not read the API key: {}", e);
            "<not-configured>".into()
        }
    }
}

async fn run(
    ctx: &Arc<AppState>,
    mut req: Request<Body>,
    timeout: Duration,
) -> Result<Raw, Transport> {
    if let Ok(budget) = CALLER.try_with(|b| b.clone()) {
        if !budget.charge(ctx) {
            return Err(Transport::Budget);
        }
    }
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(MCP_CALLER, 0)));
    let router = crate::server::api_v1::router()
        .route_layer(mw::from_fn_with_state(
            ctx.clone(),
            crate::server::middleware::api_rate_limit,
        ))
        .fallback(crate::server::api_v1::api_not_found)
        .with_state(ctx.clone());
    let (tx, rx) = tokio::sync::oneshot::channel();
    ctx.spawn(async move {
        use futures_util::FutureExt;
        // From here the request is in the handlers' hands: whatever goes
        // wrong may come after an order reached the broker (MCP-01).
        let out = match std::panic::AssertUnwindSafe(router.oneshot(req))
            .catch_unwind()
            .await
        {
            Ok(Ok(resp)) => {
                let status = resp.status().as_u16();
                let outcome_unknown = resp
                    .extensions()
                    .get::<crate::brokers::common::outcome::PlaceOutcome>()
                    .is_some_and(|p| p.is_uncertain());
                match Limited::new(resp.into_body(), MAX_REPLY_BYTES)
                    .collect()
                    .await
                {
                    Ok(b) => Some(Raw {
                        status,
                        text: String::from_utf8_lossy(&b.to_bytes()).into_owned(),
                        outcome_unknown,
                    }),
                    Err(e) => {
                        tracing::error!("MCP could not read an API reply: {}", e);
                        None
                    }
                }
            }
            Ok(Err(never)) => match never {},
            Err(_) => {
                tracing::error!("An API handler failed while serving an MCP call");
                None
            }
        };
        let _ = tx.send(out);
    });
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Some(raw))) => Ok(raw),
        // The handlers ran (or were cut off running): never "not sent".
        Ok(_) => Err(Transport::NoReply),
        Err(_) => Err(Transport::Timeout),
    }
}

/// POST `/api/v1/<endpoint>` with `payload` plus the API key.
pub async fn post_raw(
    ctx: &Arc<AppState>,
    endpoint: &str,
    mut payload: Map<String, Value>,
) -> Result<Raw, Transport> {
    payload.insert("apikey".into(), json!(api_key(ctx)));
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/v1/{}", endpoint))
        .header("content-type", "application/json")
        .body(Body::from(Value::Object(payload).to_string()))
        .map_err(|_| Transport::NotSent)?;
    run(ctx, req, SDK_TIMEOUT).await
}

/// GET `/api/v1/<endpoint>?apikey=..&<query>`.
pub async fn get_raw(
    ctx: &Arc<AppState>,
    endpoint: &str,
    query: &[(&str, &str)],
) -> Result<Raw, Transport> {
    let key = api_key(ctx);
    let mut pairs: Vec<(&str, &str)> = vec![("apikey", key.as_str())];
    pairs.extend_from_slice(query);
    let qs = serde_urlencoded::to_string(&pairs).unwrap_or_default();
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/v1/{}?{}", endpoint, qs))
        .body(Body::empty())
        .map_err(|_| Transport::NotSent)?;
    run(ctx, req, SDK_TIMEOUT).await
}

/// The SDK's dict for a transport failure (`_make_request`'s except arms).
pub fn transport_reply(t: Transport) -> Value {
    match t {
        Transport::Timeout => json!({
            "status": "error",
            "message": "Request timed out. The server took too long to respond.",
            "error_type": "timeout_error",
        }),
        Transport::NotSent => json!({
            "status": "error",
            "message": "Failed to connect to the server. Please check if the server is running.",
            "error_type": "connection_error",
        }),
        Transport::NoReply => json!({
            "status": "error",
            "message": "OpenAlgo received the request, but no answer came back.",
            "error_type": UNKNOWN_OUTCOME,
        }),
        Transport::Budget => json!({
            "status": "error",
            "message": "This AI client has made too many requests to OpenAlgo in the last minute. Wait a minute and try again.",
            "error_type": "rate_limited",
        }),
    }
}

/// `error_type` of a call that reached OpenAlgo and whose effect is
/// unknown: an order placement the broker did not confirm, or a call whose
/// answer was lost (LOG-08, MCP-01). Never safe to retry blindly.
pub const UNKNOWN_OUTCOME: &str = "unknown_outcome";

/// The SDK's `_handle_response`: non-200 becomes `HTTP <code>: <body>`, a
/// 200 error body is reduced to its message, anything else passes through.
/// An order placement with no definite answer from the broker keeps the
/// API's message under the `unknown_outcome` type instead.
pub fn shape(raw: &Raw) -> Value {
    if raw.outcome_unknown {
        let message = serde_json::from_str::<Value>(&raw.text)
            .ok()
            .and_then(|v| v.get("message").cloned())
            .unwrap_or_else(|| json!(raw.text));
        return json!({
            "status": "error",
            "message": message,
            "code": raw.status,
            "error_type": UNKNOWN_OUTCOME,
        });
    }
    if raw.status != 200 {
        return json!({
            "status": "error",
            "message": format!("HTTP {}: {}", raw.status, raw.text),
            "code": raw.status,
            "error_type": "http_error",
        });
    }
    let data: Value = match serde_json::from_str(&raw.text) {
        Ok(v) => v,
        Err(_) => {
            return json!({
                "status": "error",
                "message": "Invalid JSON response from server",
                "raw_response": raw.text,
                "error_type": "json_error",
            })
        }
    };
    if data.get("status").and_then(Value::as_str) == Some("error") {
        return json!({
            "status": "error",
            "message": data.get("message").cloned().unwrap_or(json!("Unknown error")),
            "code": raw.status,
            "error_type": "api_error",
        });
    }
    data
}

/// SDK `_make_request(endpoint, payload)`: POST and shape.
pub async fn sdk_post(ctx: &Arc<AppState>, endpoint: &str, payload: Map<String, Value>) -> Value {
    match post_raw(ctx, endpoint, payload).await {
        Ok(raw) => shape(&raw),
        Err(t) => transport_reply(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_shapes_replies() {
        let ok = shape(&Raw::new(200, r#"{"status":"success","orderid":"1"}"#));
        assert_eq!(ok["orderid"], "1");
        let api = shape(&Raw::new(
            200,
            r#"{"status":"error","message":"No position"}"#,
        ));
        assert_eq!(
            api,
            json!({"status": "error", "message": "No position", "code": 200, "error_type": "api_error"})
        );
        let http = shape(&Raw::new(
            403,
            r#"{"message":"Invalid openalgo apikey","status":"error"}"#,
        ));
        assert_eq!(http["code"], 403);
        assert_eq!(http["error_type"], "http_error");
        assert_eq!(
            http["message"],
            r#"HTTP 403: {"message":"Invalid openalgo apikey","status":"error"}"#
        );
        assert_eq!(
            transport_reply(Transport::Timeout)["error_type"],
            "timeout_error"
        );
    }

    #[test]
    fn only_a_request_never_handed_over_reads_as_not_sent() {
        // MCP-01: before dispatch it is a connection failure ...
        assert_eq!(
            transport_reply(Transport::NotSent)["error_type"],
            "connection_error"
        );
        // ... after it, the outcome is unknown.
        assert_eq!(
            transport_reply(Transport::NoReply)["error_type"],
            UNKNOWN_OUTCOME
        );
        // An order the broker did not confirm keeps the API's advice.
        let mut raw = Raw::new(
            500,
            r#"{"status":"error","message":"Check the order book before placing it again."}"#,
        );
        raw.outcome_unknown = true;
        let v = shape(&raw);
        assert_eq!(v["error_type"], UNKNOWN_OUTCOME);
        assert_eq!(
            v["message"],
            "Check the order book before placing it again."
        );
    }
}
