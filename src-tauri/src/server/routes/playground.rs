//! Web `blueprints/playground.py`: the API Playground's key and endpoint
//! list. The list is the web's own parse of its Bruno collections, generated
//! by `scripts/gen_playground_endpoints.py` (API keys cleared there).

use crate::server::middleware::User;
use crate::server::routes::webui::ok;
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

const IN_STOCK: &str = include_str!("../../../resources/playground/endpoints_IN_stock.json");
const CRYPTO: &str = include_str!("../../../resources/playground/endpoints_crypto.json");

/// GET /playground/api-key
pub async fn api_key(State(ctx): Ctx, User(_u): User) -> Response {
    let key = ApiKeyService::current(&ctx)
        .ok()
        .flatten()
        .map(|k| k.expose().to_string())
        .unwrap_or_default();
    // Never cached (security review SEC-06).
    crate::server::routes::webui::no_store(ok(json!({"api_key": key})))
}

/// GET /playground/endpoints: grouped endpoints, field order preserved.
/// WebSocket examples point at this app's feed address.
pub async fn endpoints(State(ctx): Ctx, headers: HeaderMap) -> Response {
    let crypto = ctx
        .get_broker_session()
        .map(|b| b.broker_id == "deltaexchange")
        .unwrap_or(false);
    let raw = if crypto { CRYPTO } else { IN_STOCK };
    let ws = crate::server::routes::websocket_example::websocket_url(&ctx, &headers);
    let body = raw.replace("ws://127.0.0.1:8765", &ws);
    let mut r = (StatusCode::OK, body).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}
