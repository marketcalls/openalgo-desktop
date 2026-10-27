//! `/api/v1/telegram/*` bot management (web `restx_api/telegram_bot.py`);
//! `telegram/notify` is in `notify.rs`. A valid key is enough, no broker
//! session. GET endpoints take the key from the `X-API-KEY` header or the
//! `apikey` query parameter, POST endpoints from the body or the header,
//! and a bad key is 401 "Invalid or missing API key", as on the web. The
//! webhook takes no key: it is verified by Telegram's secret header. What
//! happens next is `services::telegram_admin_service`.

use super::notify::{body_and_key, BAD_KEY};
use super::{authorize, query_map, send, Auth};
use crate::messaging::telegram::db::UserFilter;
use crate::server::envelope::{error, read_json_object, EarlyReply};
use crate::server::middleware::ClientIp;
use crate::services::telegram_admin_service as admin;
use crate::state::AppState;
use axum::{
    extract::{RawQuery, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value};
use std::net::IpAddr;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// The query of a GET, its key taken from the header or the query and
/// checked (401 otherwise).
async fn query_and_key(
    ctx: &AppState,
    ip: IpAddr,
    headers: &HeaderMap,
    q: Option<String>,
) -> Result<Map<String, Value>, EarlyReply> {
    let args = query_map(q);
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .filter(|k| !k.is_empty())
        .or_else(|| args.get("apikey").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();
    if key.is_empty() || !authorize(ctx, &key, ip, Auth::KeyOnly).await {
        return Err(Box::new(error(StatusCode::UNAUTHORIZED, BAD_KEY)));
    }
    Ok(args)
}

/// The body of a POST that may come without one (start, stop).
async fn optional_body_and_key(ctx: &AppState, ip: IpAddr, req: Request) -> Result<(), EarlyReply> {
    if req.headers().contains_key(header::CONTENT_TYPE) {
        return body_and_key(ctx, ip, req).await.map(|_| ());
    }
    let key = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if key.is_empty() || !authorize(ctx, &key, ip, Auth::KeyOnly).await {
        return Err(Box::new(error(StatusCode::UNAUTHORIZED, BAD_KEY)));
    }
    Ok(())
}

/// GET /api/v1/telegram/config
pub async fn config(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Response {
    match query_and_key(&ctx, ip, &headers, q).await {
        Ok(_) => send(admin::config_reply(&ctx)),
        Err(r) => *r,
    }
}

/// POST /api/v1/telegram/config
pub async fn update_config(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match body_and_key(&ctx, ip, req).await {
        Ok(body) => send(admin::update_config_reply(&ctx, &body)),
        Err(r) => *r,
    }
}

/// POST /api/v1/telegram/start
pub async fn start(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match optional_body_and_key(&ctx, ip, req).await {
        Ok(()) => send(admin::start_reply(&ctx).await),
        Err(r) => *r,
    }
}

/// POST /api/v1/telegram/stop
pub async fn stop(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match optional_body_and_key(&ctx, ip, req).await {
        Ok(()) => send(admin::stop_reply(&ctx).await),
        Err(r) => *r,
    }
}

/// GET /api/v1/telegram/users?broker=&notifications_enabled=
pub async fn users(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Response {
    let args = match query_and_key(&ctx, ip, &headers, q).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let text = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    let filter = UserFilter {
        broker: text("broker").map(String::from),
        notifications_enabled: text("notifications_enabled")
            .map(|v| v.eq_ignore_ascii_case("true")),
    };
    send(admin::users_reply(&ctx, &filter))
}

/// POST /api/v1/telegram/broadcast
pub async fn broadcast(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match body_and_key(&ctx, ip, req).await {
        Ok(body) => send(admin::broadcast_reply(&ctx, &body).await),
        Err(r) => *r,
    }
}

/// GET /api/v1/telegram/stats?days=
pub async fn stats(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Response {
    match query_and_key(&ctx, ip, &headers, q).await {
        Ok(args) => {
            // A year of the command log at most, read on the blocking pool.
            let days = args.get("days").and_then(Value::as_str).map(String::from);
            let read = ctx.blocking(move |c| Ok(admin::stats_reply(c, days.as_deref())));
            send(read.await.unwrap_or_else(|e| {
                tracing::error!("Reading Telegram command stats failed: {}", e);
                crate::services::core::Reply::error(500, "Failed to get statistics")
            }))
        }
        Err(r) => *r,
    }
}

/// GET /api/v1/telegram/preferences?telegram_id=
pub async fn preferences(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Response {
    match query_and_key(&ctx, ip, &headers, q).await {
        Ok(args) => {
            let id = args
                .get("telegram_id")
                .and_then(Value::as_str)
                .and_then(|v| v.trim().parse::<i64>().ok());
            send(admin::preferences_reply(&ctx, id))
        }
        Err(r) => *r,
    }
}

/// POST /api/v1/telegram/preferences
pub async fn update_preferences(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match body_and_key(&ctx, ip, req).await {
        Ok(body) => send(admin::update_preferences_reply(&ctx, &body)),
        Err(r) => *r,
    }
}

/// POST /api/v1/telegram/webhook: plain text, as the web answers Telegram.
pub async fn webhook(State(ctx): Ctx, req: Request) -> Response {
    let secret = req
        .headers()
        .get("x-telegram-bot-api-secret-token")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let update = read_json_object(req, &()).await.ok().map(Value::Object);
    let (status, text) = admin::webhook_answer(&ctx, secret.as_deref(), update.as_ref());
    let mut r = (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), text).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    r
}
