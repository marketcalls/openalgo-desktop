//! Web `blueprints/whatsapp.py`: the `/whatsapp` page's JSON routes. All
//! need the signed-in user; the work is in `messaging::whatsapp`.

use crate::messaging::whatsapp::{db, normalize_phone, page, phone_to_jid, WhatsAppService};
use crate::server::envelope::{error, json_response};
use crate::server::middleware::User;
use crate::server::routes::webui::{ok, JsonBody};
use crate::services::auth_service::AuthService;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

pub const NOT_READY_MESSAGE: &str =
    "WhatsApp is not paired. Pair the device first to send messages.";

fn not_ready(ctx: &AppState) -> Response {
    error(
        StatusCode::CONFLICT,
        ctx.messaging
            .whatsapp
            .unavailable_reason()
            .unwrap_or(NOT_READY_MESSAGE),
    )
}

fn status_of(ok_: bool, msg: &str, code_ok: StatusCode, code_err: StatusCode) -> Response {
    json_response(
        if ok_ { code_ok } else { code_err },
        json!({"status": if ok_ { "success" } else { "error" }, "message": msg}),
    )
}

/// GET /whatsapp/config
pub async fn get_config(State(ctx): Ctx, _u: User) -> Response {
    let svc = &ctx.messaging.whatsapp;
    let mut cfg = WhatsAppService::config(&ctx).to_json();
    cfg.insert("is_running".into(), json!(svc.is_running()));
    cfg.insert("status_message".into(), json!(svc.unavailable_reason()));
    ok(json!({"status": "success", "data": {
        "config": Value::Object(cfg),
        "pair_state": svc.pair_state().to_json(),
    }}))
}

/// POST /whatsapp/config
pub async fn update_config(State(ctx): Ctx, _u: User, body: JsonBody) -> Response {
    let mut updates = Map::new();
    for k in [
        "broadcast_enabled",
        "rate_limit_per_minute",
        "max_message_length",
    ] {
        if let Some(v) = body.0.get(k) {
            updates.insert(k.into(), v.clone());
        }
    }
    let good = page::update_config(&ctx, &updates)
        .map_err(|e| tracing::error!("Saving the WhatsApp settings failed: {}", e))
        .is_ok();
    ok(json!({
        "status": if good { "success" } else { "error" },
        "message": if good { "Configuration updated" } else { "Failed to update" },
    }))
}

/// POST /whatsapp/pair
pub async fn pair(State(ctx): Ctx, User(u): User, body: JsonBody) -> Response {
    let phone = normalize_phone(body.0.get("phone").unwrap_or(&Value::Null));
    let owner_id = AuthService::user_id(&ctx, &u.username).ok().flatten();
    let svc = &ctx.messaging.whatsapp;
    let (good, msg) = svc
        .start_pair(
            &ctx,
            (!phone.is_empty()).then_some(phone),
            owner_id,
            Some(u.username.clone()),
        )
        .await;
    json_response(
        if good {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        json!({
            "status": if good { "success" } else { "error" },
            "message": msg,
            "data": svc.pair_state().to_json(),
        }),
    )
}

/// GET /whatsapp/pair/status
pub async fn pair_status(State(ctx): Ctx, _u: User) -> Response {
    ok(json!({"status": "success", "data": ctx.messaging.whatsapp.pair_state().to_json()}))
}

/// POST /whatsapp/unlink
pub async fn unlink(State(ctx): Ctx, _u: User) -> Response {
    let (good, msg) = ctx.messaging.whatsapp.unlink(&ctx).await;
    status_of(
        good,
        &msg,
        StatusCode::OK,
        StatusCode::INTERNAL_SERVER_ERROR,
    )
}

/// POST /whatsapp/bot/start
pub async fn bot_start(State(ctx): Ctx, _u: User) -> Response {
    let (good, msg) = ctx.messaging.whatsapp.start_bot(&ctx).await;
    status_of(good, &msg, StatusCode::OK, StatusCode::BAD_REQUEST)
}

/// POST /whatsapp/bot/stop
pub async fn bot_stop(State(ctx): Ctx, _u: User) -> Response {
    let (good, msg) = ctx.messaging.whatsapp.stop_bot(&ctx).await;
    status_of(
        good,
        &msg,
        StatusCode::OK,
        StatusCode::INTERNAL_SERVER_ERROR,
    )
}

/// GET /whatsapp/bot/status
pub async fn bot_status(State(ctx): Ctx, _u: User) -> Response {
    let cfg = WhatsAppService::config(&ctx);
    let svc = &ctx.messaging.whatsapp;
    ok(json!({"status": "success", "data": {
        "is_running": svc.is_running(),
        "is_paired": cfg.is_paired,
        "is_active": cfg.is_active,
        "own_jid": cfg.own_jid,
        "own_phone": cfg.own_phone,
        "bot_username": cfg.bot_username,
        "paired_at": crate::messaging::format::http_date(cfg.paired_at.as_deref()),
        "status_message": svc.unavailable_reason(),
    }}))
}

/// GET /whatsapp/users
pub async fn users(State(ctx): Ctx, _u: User) -> Response {
    match page::users(&ctx) {
        Ok(list) => {
            let n = list.len();
            ok(json!({"status": "success",
                "data": list.iter().map(db::WaUser::to_json).collect::<Vec<_>>(),
                "count": n}))
        }
        Err(e) => {
            tracing::error!("Listing WhatsApp users failed: {}", e);
            error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to list users")
        }
    }
}

/// POST /whatsapp/user/{jid}/unlink
pub async fn unlink_user(State(ctx): Ctx, _u: User, Path(jid): Path<String>) -> Response {
    let good = page::unlink_user(&ctx, &jid).unwrap_or(false);
    json_response(
        if good {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        },
        json!({"status": if good { "success" } else { "error" },
               "message": if good { "User unlinked" } else { "User not found" }}),
    )
}

/// POST /whatsapp/broadcast
pub async fn broadcast(State(ctx): Ctx, _u: User, body: JsonBody) -> Response {
    let svc = &ctx.messaging.whatsapp;
    if !svc.is_ready(&ctx) {
        return not_ready(&ctx);
    }
    let message = body.0.get("message").and_then(Value::as_str).unwrap_or("");
    if message.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Message is required");
    }
    if !WhatsAppService::config(&ctx).broadcast_enabled {
        return error(StatusCode::FORBIDDEN, "Broadcast is disabled");
    }
    let f = body.0.get("filters");
    let broker = f
        .and_then(|f| f.get("broker"))
        .and_then(Value::as_str)
        .map(String::from);
    let notif = f
        .and_then(|f| f.get("notifications_enabled"))
        .and_then(Value::as_bool);
    let (queued, skipped) = svc.broadcast(&ctx, message, broker, notif);
    ok(json!({
        "status": "success",
        "message": format!("Queued for {} users, skipped {}", queued, skipped),
        "queued": queued,
        "skipped": skipped,
    }))
}

/// Send on a task owned by the app context (web `alert_executor.submit`).
fn queue_send(ctx: &Arc<AppState>, jid: String, text: String) {
    let weak = Arc::downgrade(ctx);
    ctx.spawn(async move {
        if let Some(c) = weak.upgrade() {
            c.messaging.whatsapp.send_alert(&c, &[jid], &text).await;
        }
    });
}

/// POST /whatsapp/test-message
pub async fn test_message(State(ctx): Ctx, User(u): User) -> Response {
    if !ctx.messaging.whatsapp.is_ready(&ctx) {
        return not_ready(&ctx);
    }
    let mine = page::user_by_username(&ctx, &u.username).ok().flatten();
    let (jid, text) = match mine {
        Some(m) => (
            m.whatsapp_jid,
            "*Test from OpenAlgo*\nYour WhatsApp integration is working.".to_string(),
        ),
        None => {
            let all = page::users(&ctx).unwrap_or_default();
            let Some(first) = all.into_iter().next() else {
                return error(
                    StatusCode::NOT_FOUND,
                    "No linked WhatsApp users. Ask a user to send /link <api_key> to the bot first, or pair this number to receive admin alerts.",
                );
            };
            (
                first.whatsapp_jid,
                format!(
                    "*Test from OpenAlgo (admin: {})*\nYour WhatsApp integration is working.",
                    u.username
                ),
            )
        }
    };
    queue_send(&ctx, jid.clone(), text);
    ok(json!({"status": "success", "message": format!("Test queued to {}", jid)}))
}

/// POST /whatsapp/send
pub async fn send(State(ctx): Ctx, _u: User, body: JsonBody) -> Response {
    if !ctx.messaging.whatsapp.is_ready(&ctx) {
        return not_ready(&ctx);
    }
    let b = &body.0;
    let phone = normalize_phone(b.get("phone").unwrap_or(&Value::Null));
    let message = b.get("message").and_then(Value::as_str).unwrap_or("");
    if phone.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Phone number is required");
    }
    if message.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Message is required");
    }
    // The desktop keeps no attachment folder, so no server-side file may be
    // sent (the web's allowlist with nothing in it).
    for k in ["image_path", "document_path"] {
        if crate::messaging::format::truthy(b.get(k)) {
            return error(StatusCode::BAD_REQUEST, format!("{} is not allowed", k));
        }
    }
    let jid = phone_to_jid(&phone);
    queue_send(&ctx, jid.clone(), message.to_string());
    ok(json!({"status": "success", "message": format!("Queued to {}", jid)}))
}

const STATS_UNREADABLE: &str =
    "The WhatsApp command statistics could not be read just now. Try again.";

/// GET /whatsapp/stats?days=
pub async fn stats(
    State(ctx): Ctx,
    _u: User,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let days = q
        .get("days")
        .and_then(|d| d.trim().parse::<i64>().ok())
        .map(|d| d.clamp(1, 365))
        .unwrap_or(7);
    // Up to a year of the command log (kept forever as on the web), read on
    // the blocking pool a report at a time (AppState::report). Desktop: a
    // log that cannot be read says so; the web answers zero commands, which
    // reads as a bot nobody used (lead, 2026-10-20).
    match ctx.report(move |c| page::command_stats(c, days)).await {
        Ok(data) => ok(json!({"status": "success", "data": data})),
        Err(e) => {
            tracing::error!("Reading WhatsApp stats failed: {}", e);
            error(StatusCode::INTERNAL_SERVER_ERROR, STATS_UNREADABLE)
        }
    }
}

/// The WhatsApp page routes (web `blueprints/whatsapp.py`), each with its
/// access rule.
pub fn table() -> Vec<super::RouteSpec> {
    vec![
        r!(GET, "/whatsapp/config", User, get_config),
        r!(POST, "/whatsapp/config", User, update_config),
        r!(POST, "/whatsapp/pair", User, pair),
        r!(GET, "/whatsapp/pair/status", User, pair_status),
        r!(POST, "/whatsapp/unlink", User, unlink),
        r!(POST, "/whatsapp/bot/start", User, bot_start),
        r!(POST, "/whatsapp/bot/stop", User, bot_stop),
        r!(GET, "/whatsapp/bot/status", User, bot_status),
        r!(GET, "/whatsapp/users", User, users),
        r!(POST, "/whatsapp/user/{jid}/unlink", User, unlink_user),
        r!(POST, "/whatsapp/broadcast", User, broadcast),
        r!(POST, "/whatsapp/test-message", User, test_message),
        r!(POST, "/whatsapp/send", User, send),
        r!(GET, "/whatsapp/stats", User, stats),
    ]
}
