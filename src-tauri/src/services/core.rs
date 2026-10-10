//! Shared plumbing for the `/api/v1` services: the reply type (status code +
//! JSON body, like the web's `(success, response_data, status_code)`), the
//! connected broker handle, the analyzer-mode check and event helpers.

use crate::brokers::common::outcome::PlaceOutcome;
use crate::brokers::types::AuthToken;
use crate::brokers::Broker;
use crate::events::{Event, Mode, OrderMeta};
use crate::sandbox::SandboxError;
use crate::state::AppState;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// A service outcome: HTTP status and JSON body, exactly as sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    /// What a live placement did at the broker (LOG-08). Never serialised:
    /// the wire keeps the web's shape, while in-process callers (strategies,
    /// scalping, OpenScript, MCP) tell an uncertain placement from a
    /// refusal by this, not by the status code.
    pub placement: Option<PlaceOutcome>,
}

impl Reply {
    pub fn new(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            placement: None,
        }
    }

    /// This reply carries what a live placement did.
    pub fn with_placement(mut self, outcome: PlaceOutcome) -> Self {
        self.placement = Some(outcome);
        self
    }

    /// The placement may have reached the broker without a definite answer.
    pub fn is_uncertain(&self) -> bool {
        self.placement
            .as_ref()
            .is_some_and(PlaceOutcome::is_uncertain)
    }

    pub fn ok(body: Value) -> Self {
        Self::new(200, body)
    }

    /// `{"status":"error","message":msg}`.
    pub fn error(status: u16, msg: impl Into<String>) -> Self {
        Self::new(status, json!({"status": "error", "message": msg.into()}))
    }

    /// `{"mode":"analyze","status":"error","message":msg}`.
    pub fn analyze_error(status: u16, msg: impl Into<String>) -> Self {
        Self::new(
            status,
            json!({"mode": "analyze", "status": "error", "message": msg.into()}),
        )
    }

    /// A serialisable sandbox reply with HTTP 200.
    pub fn from_ser<T: Serialize>(v: &T) -> Self {
        match serde_json::to_value(v) {
            Ok(b) => Self::ok(b),
            Err(e) => {
                tracing::error!("Could not serialise a reply: {}", e);
                Self::error(500, UNEXPECTED)
            }
        }
    }

    /// A sandbox refusal with its own status and the analyze-mode body.
    pub fn sandbox(e: &SandboxError) -> Self {
        let body = serde_json::to_value(e.body()).unwrap_or_else(
            |_| json!({"status": "error", "message": e.message, "mode": "analyze"}),
        );
        Self::new(e.http_status, body)
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The `message` field, when it is a string.
    pub fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

pub const UNEXPECTED: &str = "An unexpected error occurred";
pub const INVALID_API_KEY: &str = "Invalid openalgo apikey";
pub const BROKER_MODULE_NOT_FOUND: &str = "Broker-specific module not found";

/// The connected broker and its session token.
#[derive(Clone)]
pub struct BrokerHandle {
    pub id: String,
    pub broker: Arc<dyn Broker>,
    pub auth: AuthToken,
}

/// The live broker session, or the web's answer when there is none
/// (403 invalid key: the web reports a missing broker token that way) or
/// the adapter is not compiled in (404).
pub fn broker_handle(ctx: &AppState) -> Result<BrokerHandle, Reply> {
    let Some(session) = ctx.get_broker_session() else {
        return Err(Reply::error(403, INVALID_API_KEY));
    };
    let Some(broker) = ctx.brokers.get(&session.broker_id) else {
        return Err(Reply::error(404, BROKER_MODULE_NOT_FOUND));
    };
    let mut auth =
        AuthToken::new(session.auth_token.expose()).with_user_id(session.user_id.clone());
    if let Some(f) = session.feed_token.as_ref() {
        auth = auth.with_feed(Some(f.expose().to_string()));
    }
    Ok(BrokerHandle {
        id: session.broker_id,
        broker,
        auth,
    })
}

/// Analyzer (sandbox) mode is on.
pub fn is_analyze(ctx: &AppState) -> bool {
    match ctx.sqlite.get_analyze_mode() {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("Could not read analyzer mode: {}", e);
            false
        }
    }
}

pub fn mode_of(analyze: bool) -> Mode {
    if analyze {
        Mode::Analyze
    } else {
        Mode::Live
    }
}

/// The request as logged and published: `apikey` removed.
pub fn safe_request(request: &Value) -> Value {
    let mut v = request.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("apikey");
    }
    v
}

/// The analyzer copy of a request: `apikey` removed, `api_type` added.
pub fn analyzer_request(request: &Value, api_type: &str) -> Value {
    let mut v = safe_request(request);
    if let Some(m) = v.as_object_mut() {
        m.insert("api_type".into(), json!(api_type));
    }
    v
}

pub fn meta(mode: Mode, api_type: &str, request: Value, response: &Value) -> OrderMeta {
    OrderMeta {
        mode,
        api_type: api_type.to_string(),
        request_data: request,
        response_data: response.clone(),
    }
}

pub fn publish(ctx: &AppState, event: Event) {
    ctx.bus.publish(event);
}

/// Web `emit_analyzer_error`: the analyze-mode error body, published as
/// `analyzer.error`.
pub fn analyzer_error(
    ctx: &AppState,
    api_type: &str,
    request: &Value,
    msg: &str,
    status: u16,
) -> Reply {
    let reply = Reply::analyze_error(status, msg);
    publish(
        ctx,
        Event::AnalyzerError {
            meta: meta(
                Mode::Analyze,
                api_type,
                analyzer_request(request, api_type),
                &reply.body,
            ),
        },
    );
    reply
}

/// Live-mode failure published as `order.failed`.
pub fn order_failed(
    ctx: &AppState,
    api_type: &str,
    request: &Value,
    reply: &Reply,
    symbol: &str,
    exchange: &str,
) {
    publish(
        ctx,
        Event::OrderFailed {
            meta: meta(Mode::Live, api_type, safe_request(request), &reply.body),
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
            error_message: reply.message(),
        },
    );
}

/// A float that is whole prints as an integer (the web passes broker JSON
/// through, and Kite sends `0`, `1187`, `22520` as integers).
pub fn num(x: f64) -> Value {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e15 {
        json!(x as i64)
    } else {
        serde_json::Number::from_f64(x)
            .map(Value::Number)
            .unwrap_or(json!(0))
    }
}

/// A float rounded to two decimals, always a float (`round(x, 2)`).
pub fn round2(x: f64) -> f64 {
    crate::brokers::common::mpp::py_round(x, 2)
}

/// JSON float (never an integer).
pub fn float(x: f64) -> Value {
    serde_json::Number::from_f64(x)
        .map(Value::Number)
        .unwrap_or(json!(0.0))
}

/// Read a string field of a loaded body.
pub fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

pub fn i(v: &Value, k: &str) -> i64 {
    v.get(k)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_like_broker_json() {
        assert_eq!(num(1187.0).to_string(), "1187");
        assert_eq!(num(1167.7).to_string(), "1167.7");
        assert_eq!(float(0.0).to_string(), "0.0");
        assert_eq!(round2(1.005), 1.0);
    }

    #[test]
    fn requests_never_carry_the_key() {
        let r = json!({"apikey": "secret", "symbol": "SBIN"});
        assert_eq!(safe_request(&r), json!({"symbol": "SBIN"}));
        assert_eq!(
            analyzer_request(&r, "placeorder"),
            json!({"symbol": "SBIN", "api_type": "placeorder"})
        );
    }
}
