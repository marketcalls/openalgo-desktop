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

use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request};
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// The address MCP calls carry (discard-only prefix, RFC 6666).
pub const MCP_CALLER: IpAddr = IpAddr::V6(Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 1));

/// The SDK's request timeout (`BaseAPI(timeout=120.0)`).
pub const SDK_TIMEOUT: Duration = Duration::from_secs(120);

/// What the handler stack answered.
#[derive(Debug, Clone)]
pub struct Raw {
    pub status: u16,
    pub text: String,
}

/// Why no answer arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Timeout,
    Unavailable,
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
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(MCP_CALLER, 0)));
    let router = crate::server::api_v1::router()
        .fallback(crate::server::api_v1::api_not_found)
        .with_state(ctx.clone());
    let (tx, rx) = tokio::sync::oneshot::channel();
    ctx.spawn(async move {
        let out = match router.oneshot(req).await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                match resp.into_body().collect().await {
                    Ok(b) => Some(Raw {
                        status,
                        text: String::from_utf8_lossy(&b.to_bytes()).into_owned(),
                    }),
                    Err(e) => {
                        tracing::error!("MCP could not read an API reply: {}", e);
                        None
                    }
                }
            }
            Err(never) => match never {},
        };
        let _ = tx.send(out);
    });
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Some(raw))) => Ok(raw),
        Ok(_) => Err(Transport::Unavailable),
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
        .map_err(|_| Transport::Unavailable)?;
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
        .map_err(|_| Transport::Unavailable)?;
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
        Transport::Unavailable => json!({
            "status": "error",
            "message": "Failed to connect to the server. Please check if the server is running.",
            "error_type": "connection_error",
        }),
    }
}

/// The SDK's `_handle_response`: non-200 becomes `HTTP <code>: <body>`, a
/// 200 error body is reduced to its message, anything else passes through.
pub fn shape(raw: &Raw) -> Value {
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
        let ok = shape(&Raw {
            status: 200,
            text: r#"{"status":"success","orderid":"1"}"#.into(),
        });
        assert_eq!(ok["orderid"], "1");
        let api = shape(&Raw {
            status: 200,
            text: r#"{"status":"error","message":"No position"}"#.into(),
        });
        assert_eq!(
            api,
            json!({"status": "error", "message": "No position", "code": 200, "error_type": "api_error"})
        );
        let http = shape(&Raw {
            status: 403,
            text: r#"{"message":"Invalid openalgo apikey","status":"error"}"#.into(),
        });
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
}
