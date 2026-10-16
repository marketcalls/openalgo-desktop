//! `/api/v1/telegram/notify` (web `restx_api/telegram_bot.py`) and
//! `/api/v1/whatsapp/notify` (web `restx_api/whatsapp_bot.py`).
//!
//! These two take the key from the body or the `X-API-KEY` header and
//! answer a bad key with 401 "Invalid or missing API key", as the web's
//! namespaces do (unlike the order endpoints). A valid key is enough; no
//! broker session is needed. What happens next is
//! `services::notify_service`.

use super::{authorize, send, Auth};
use crate::server::envelope::{error, read_json_object};
use crate::server::middleware::ClientIp;
use crate::services::notify_service;
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{Map, Value};
use std::net::IpAddr;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// The Telegram and WhatsApp namespaces' answer to a bad or missing key.
pub(super) const BAD_KEY: &str = "Invalid or missing API key";

/// The JSON body of a POST in these namespaces, its key taken from the
/// body or the `X-API-KEY` header and checked (401 otherwise).
pub(super) async fn body_and_key(
    ctx: &AppState,
    ip: IpAddr,
    req: Request,
) -> Result<Map<String, Value>, Response> {
    let header_key = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = read_json_object(req, &()).await?;
    let key = body
        .get("apikey")
        .and_then(Value::as_str)
        .map(String::from)
        .filter(|k| !k.is_empty())
        .or(header_key)
        .unwrap_or_default();
    if key.is_empty() || !authorize(ctx, &key, ip, Auth::KeyOnly) {
        return Err(error(StatusCode::UNAUTHORIZED, BAD_KEY));
    }
    Ok(body)
}

/// POST /api/v1/telegram/notify
pub async fn telegram_notify(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match body_and_key(&ctx, ip, req).await {
        Ok(data) => send(notify_service::telegram(&ctx, &data).await),
        Err(r) => r,
    }
}

/// POST /api/v1/whatsapp/notify
pub async fn whatsapp_notify(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match body_and_key(&ctx, ip, req).await {
        Ok(data) => send(notify_service::whatsapp(&ctx, &data).await),
        Err(r) => r,
    }
}
