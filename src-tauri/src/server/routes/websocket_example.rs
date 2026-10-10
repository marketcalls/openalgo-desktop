//! Web `blueprints/websocket_example.py`: what the in-app market data pages
//! need to open their own connection to the feed server.

use crate::server::envelope::{error, json_response};
use crate::server::middleware::User;
use crate::server::routes::webui::{failed, ok};
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use serde_json::json;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// The feed address a page should use. A saved `websocket_url` wins;
/// otherwise the configured port on loopback, or, when the trader allowed
/// other devices, on the host the page was loaded from.
pub fn websocket_url(ctx: &AppState, headers: &HeaderMap) -> String {
    let cfg = ctx.server_config();
    if let Some(u) = cfg.websocket_url.clone() {
        return u;
    }
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| {
            if let Some(rest) = h.strip_prefix('[') {
                rest.split(']')
                    .next()
                    .map(|v| format!("[{}]", v))
                    .unwrap_or_default()
            } else {
                h.rsplit_once(':').map(|(n, _)| n).unwrap_or(h).to_string()
            }
        })
        .filter(|h| !h.is_empty() && !cfg.is_loopback())
        .unwrap_or_else(|| "127.0.0.1".into());
    format!("ws://{}:{}", host, cfg.ws_port)
}

/// GET /api/websocket/config
pub async fn config(State(ctx): Ctx, headers: HeaderMap) -> Response {
    let url = websocket_url(&ctx, &headers);
    ok(json!({
        "status": "success",
        "websocket_url": url,
        "is_secure": false,
        "original_url": url,
    }))
}

/// GET /api/websocket/apikey: the key the page authenticates the feed with.
pub async fn apikey(State(ctx): Ctx, User(_u): User) -> Response {
    match ApiKeyService::current(&ctx) {
        // Never cached (security review SEC-06).
        Ok(Some(k)) => crate::server::routes::webui::no_store(json_response(
            StatusCode::OK,
            json!({"status": "success", "api_key": k.expose()}),
        )),
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "No API key found. Please generate an API key first.",
        ),
        Err(e) => failed(
            "Reading the API key",
            e,
            "Could not read your API key. Try again.",
        ),
    }
}
