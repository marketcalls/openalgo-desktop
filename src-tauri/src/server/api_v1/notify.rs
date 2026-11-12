//! `/api/v1/telegram/notify` (web `restx_api/telegram_bot.py`),
//! `/api/v1/whatsapp/notify` (web `restx_api/whatsapp_bot.py`), and
//! `/api/v1/slack/notify` and `/api/v1/discord/notify` (web
//! `restx_api/webhook_alerts.py`, marketcalls/openalgo#2435).
//!
//! These take the key from the body or the `X-API-KEY` header (never the
//! query string) and answer a bad key with 401 "Invalid or missing API
//! key", as the web's namespaces do (unlike the order endpoints). A valid
//! key is enough; no broker session is needed. What happens next is
//! `services::notify_service`.

use super::{authorize, send, Auth};
use crate::messaging::channels::webhook::WebhookKind;
use crate::messaging::format::truthy;
use crate::server::envelope::{error, is_json, read_json_object, EarlyReply};
use crate::server::middleware::ClientIp;
use crate::services::notify_service;
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{FromRequest, Request, State},
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
) -> Result<Map<String, Value>, EarlyReply> {
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
    if key.is_empty() || !authorize(ctx, &key, ip, Auth::KeyOnly).await {
        return Err(Box::new(error(StatusCode::UNAUTHORIZED, BAD_KEY)));
    }
    Ok(body)
}

/// POST /api/v1/telegram/notify
pub async fn telegram_notify(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match body_and_key(&ctx, ip, req).await {
        Ok(data) => send(notify_service::telegram(&ctx, &data).await),
        Err(r) => *r,
    }
}

/// POST /api/v1/whatsapp/notify
pub async fn whatsapp_notify(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match body_and_key(&ctx, ip, req).await {
        Ok(data) => send(notify_service::whatsapp(&ctx, &data).await),
        Err(r) => *r,
    }
}

/// A Slack or Discord notify body (web `request.get_json(silent=True)`):
/// anything that is not a JSON object is one 400, before the key is read;
/// then the key from the body (any value the web reads as true) or the
/// `X-API-KEY` header, checked (401 otherwise).
async fn webhook_body_and_key(
    ctx: &AppState,
    ip: IpAddr,
    req: Request,
) -> Result<Map<String, Value>, EarlyReply> {
    let not_an_object = || {
        Box::new(error(
            StatusCode::BAD_REQUEST,
            "Send the request as a JSON object.",
        ))
    };
    let header_key = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    if !is_json(&req) {
        return Err(not_an_object());
    }
    let bytes = Bytes::from_request(req, &())
        .await
        .map_err(|_| not_an_object())?;
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&bytes) else {
        return Err(not_an_object());
    };
    let key = match body.get("apikey") {
        Some(given) if truthy(Some(given)) => given.as_str().map(String::from),
        _ => header_key,
    };
    let key = key.filter(|k| !k.is_empty()).unwrap_or_default();
    if key.is_empty() || !authorize(ctx, &key, ip, Auth::KeyOnly).await {
        return Err(Box::new(error(StatusCode::UNAUTHORIZED, BAD_KEY)));
    }
    Ok(body)
}

async fn webhook_notify(
    ctx: Arc<AppState>,
    ip: IpAddr,
    req: Request,
    kind: WebhookKind,
) -> Response {
    match webhook_body_and_key(&ctx, ip, req).await {
        Ok(data) => send(notify_service::webhook(&ctx, kind, &data).await),
        Err(r) => *r,
    }
}

/// POST /api/v1/slack/notify
pub async fn slack_notify(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    webhook_notify(ctx, ip, req, WebhookKind::Slack).await
}

/// POST /api/v1/discord/notify
pub async fn discord_notify(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    webhook_notify(ctx, ip, req, WebhookKind::Discord).await
}
