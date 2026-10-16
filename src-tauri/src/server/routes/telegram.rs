//! Web `blueprints/telegram.py`: the `/telegram` page's JSON routes. All
//! need the signed-in user; the bot work is in `messaging::telegram`.

use crate::messaging::telegram::db::{self, ConfigUpdate, UserFilter};
use crate::messaging::telegram::{manage, page};
use crate::server::envelope::error;
use crate::server::middleware::User;
use crate::server::routes::webui::{ok, JsonBody};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

const FAILED: &str = manage::SETTINGS_UNREADABLE;

/// The bot settings; a failed read is logged.
fn config(ctx: &AppState) -> Option<db::BotConfig> {
    page::bot_config(ctx)
        .map_err(|e| tracing::error!("Reading the Telegram settings failed: {}", e))
        .ok()
}

fn config_unreadable() -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, FAILED)
}

fn users(ctx: &AppState, f: &UserFilter) -> Vec<Value> {
    page::users(ctx, f)
        .map(|u| u.iter().map(db::TgUser::list_json).collect())
        .unwrap_or_else(|e| {
            tracing::error!("Reading Telegram users failed: {}", e);
            vec![]
        })
}

/// `_format_stats_for_react(get_command_stats(days))`.
fn stats(ctx: &AppState, days: i64) -> (Vec<Value>, i64, i64) {
    match page::command_stats(ctx, days) {
        Ok(s) => (
            s.commands_by_type
                .iter()
                .map(|(c, n)| json!({"command": c, "count": n}))
                .collect(),
            s.total_commands,
            s.active_users,
        ),
        Err(e) => {
            tracing::error!("Reading Telegram command stats failed: {}", e);
            (vec![], 0, 0)
        }
    }
}

fn filter_from(body: &JsonBody) -> UserFilter {
    let f = body.0.get("filters");
    UserFilter {
        broker: f
            .and_then(|f| f.get("broker"))
            .and_then(Value::as_str)
            .map(String::from),
        notifications_enabled: f
            .and_then(|f| f.get("notifications_enabled"))
            .and_then(Value::as_bool),
    }
}

/// POST /telegram/config
pub async fn configuration(State(ctx): Ctx, _u: User, body: JsonBody) -> Response {
    let b = &body.0;
    let mut u = ConfigUpdate::default();
    if let Some(t) = b.get("token") {
        u.token = Some(
            t.as_str()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    if let Some(v) = b.get("broadcast_enabled") {
        u.broadcast_enabled = Some(crate::messaging::format::truthy(Some(v)));
    }
    if let Some(v) = b.get("rate_limit_per_minute") {
        match crate::messaging::format::py_int(v) {
            Some(n) => u.rate_limit_per_minute = Some(n),
            None => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "The rate limit must be a whole number.",
                )
            }
        }
    }
    match page::update_config(&ctx, u) {
        Ok(()) => ok(json!({"status": "success", "message": "Configuration updated"})),
        Err(e) => {
            tracing::error!("Saving the Telegram settings failed: {}", e);
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to update configuration",
            )
        }
    }
}

/// POST /telegram/bot/start
pub async fn bot_start(State(ctx): Ctx, _u: User) -> Response {
    match manage::start_bot(&ctx).await {
        Ok(msg) => ok(json!({"status": "success", "message": msg})),
        Err((status, msg)) => error(
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            msg,
        ),
    }
}

/// POST /telegram/bot/stop
pub async fn bot_stop(State(ctx): Ctx, _u: User) -> Response {
    let (good, msg) = ctx.messaging.telegram.stop(&ctx).await;
    if good {
        ok(json!({"status": "success", "message": msg}))
    } else {
        error(StatusCode::INTERNAL_SERVER_ERROR, msg)
    }
}

fn bot_status_json(ctx: &AppState, cfg: &db::BotConfig) -> Value {
    json!({
        "is_running": ctx.messaging.telegram.is_running(),
        "is_configured": cfg.token.is_some(),
        "bot_username": cfg.bot_username,
        "is_active": cfg.is_active,
    })
}

/// GET /telegram/bot/status
pub async fn bot_status(State(ctx): Ctx, _u: User) -> Response {
    match config(&ctx) {
        Some(c) => ok(json!({"status": "success", "data": bot_status_json(&ctx, &c)})),
        None => config_unreadable(),
    }
}

/// POST /telegram/broadcast
pub async fn broadcast(State(ctx): Ctx, _u: User, body: JsonBody) -> Response {
    let message = body
        .0
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if message.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Message is required");
    }
    match config(&ctx) {
        Some(c) if !c.broadcast_enabled => {
            return error(StatusCode::FORBIDDEN, "Broadcast is disabled")
        }
        Some(_) => {}
        None => return config_unreadable(),
    }
    let (sent, failed) = manage::broadcast(&ctx, &message, &filter_from(&body)).await;
    ok(json!({
        "status": "success",
        "message": format!("Sent to {} users, failed for {}", sent, failed),
        "success_count": sent,
        "fail_count": failed,
    }))
}

/// POST /telegram/user/{telegram_id}/unlink
pub async fn unlink_user(State(ctx): Ctx, _u: User, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return error(StatusCode::NOT_FOUND, "User not found");
    };
    match page::unlink_user(&ctx, id) {
        Ok(true) => ok(json!({"status": "success", "message": "User unlinked"})),
        Ok(false) => error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to unlink user"),
        Err(e) => {
            tracing::error!("Unlinking a Telegram user failed: {}", e);
            error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to unlink user")
        }
    }
}

/// POST /telegram/test-message
pub async fn test_message(State(ctx): Ctx, User(u): User) -> Response {
    let all = page::users(&ctx, &UserFilter::default()).unwrap_or_default();
    let mine = all.iter().find(|x| x.openalgo_username == u.username);
    let (target, message) = match (mine, all.first()) {
        (Some(m), _) => (
            m.telegram_id,
            "Test Message from OpenAlgo\n\nYour Telegram integration is working correctly!"
                .to_string(),
        ),
        (None, Some(first)) => (
            first.telegram_id,
            format!(
                "Test Message from OpenAlgo (Admin: {})\n\nYour Telegram integration is working correctly!",
                u.username
            ),
        ),
        (None, None) => {
            return error(
                StatusCode::NOT_FOUND,
                "No Telegram users found. Please ensure at least one user has started the bot with /start",
            )
        }
    };
    if ctx
        .messaging
        .telegram
        .send_alert(&ctx, target, &message)
        .await
    {
        ok(json!({"status": "success", "message": "Test message sent"}))
    } else {
        ok(json!({"status": "success", "message": "Test message queued for delivery"}))
    }
}

/// POST /telegram/send-message
pub async fn send_message(State(ctx): Ctx, User(u): User, body: JsonBody) -> Response {
    let id = body.0.get("telegram_id");
    let message = body.0.get("message").and_then(Value::as_str).unwrap_or("");
    if !crate::messaging::format::truthy(id) || message.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Missing telegram_id or message");
    }
    let Some(id) = id.and_then(crate::messaging::format::py_int) else {
        return error(StatusCode::BAD_REQUEST, "Invalid telegram_id");
    };
    match page::user(&ctx, id) {
        Ok(Some(_)) => {}
        _ => return error(StatusCode::NOT_FOUND, "User not found"),
    }
    if message.chars().count() > 4096 {
        return error(
            StatusCode::BAD_REQUEST,
            "Message too long (max 4096 characters)",
        );
    }
    tracing::info!("User {} sending a Telegram message", u.username);
    if ctx.messaging.telegram.send_alert(&ctx, id, message).await {
        ok(json!({"status": "success", "message": "Message sent successfully"}))
    } else {
        error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to send message")
    }
}

/// GET /telegram/api/index
pub async fn api_index(State(ctx): Ctx, User(u): User) -> Response {
    let Some(cfg) = config(&ctx) else {
        return config_unreadable();
    };
    let (stats7, total, active) = stats(&ctx, 7);
    let me = page::user_by_username(&ctx, &u.username)
        .ok()
        .flatten()
        .map(|x| x.full_json())
        .unwrap_or(Value::Null);
    ok(json!({"status": "success", "data": {
        "bot_status": bot_status_json(&ctx, &cfg),
        "config": {
            "bot_username": cfg.bot_username,
            "broadcast_enabled": cfg.broadcast_enabled,
            "rate_limit_per_minute": cfg.rate_limit_per_minute,
            "is_active": cfg.is_active,
        },
        "users": users(&ctx, &UserFilter::default()),
        "stats": stats7,
        "total_commands": total,
        "active_users_7d": active,
        "telegram_user": me,
    }}))
}

/// GET /telegram/api/config (also the JSON side of GET /telegram/config).
pub async fn api_config(State(ctx): Ctx, _u: User) -> Response {
    match config(&ctx) {
        Some(cfg) => ok(json!({"status": "success", "data": {
            "has_token": cfg.token.is_some(),
            "bot_username": cfg.bot_username,
            "broadcast_enabled": cfg.broadcast_enabled,
            "rate_limit_per_minute": cfg.rate_limit_per_minute,
            "is_active": cfg.is_active,
        }})),
        None => config_unreadable(),
    }
}

/// GET /telegram/api/users
pub async fn api_users(State(ctx): Ctx, _u: User) -> Response {
    let (s, total, _) = stats(&ctx, 30);
    ok(json!({"status": "success", "data": {
        "users": users(&ctx, &UserFilter::default()),
        "stats": s,
        "total_commands": total,
    }}))
}

/// GET /telegram/api/analytics
pub async fn api_analytics(State(ctx): Ctx, _u: User) -> Response {
    let (s7, _, _) = stats(&ctx, 7);
    let (s30, _, _) = stats(&ctx, 30);
    let list = page::users(&ctx, &UserFilter::default()).unwrap_or_default();
    let active = list.iter().filter(|u| u.notifications_enabled).count();
    ok(json!({"status": "success", "data": {
        "stats_7d": s7,
        "stats_30d": s30,
        "total_users": list.len(),
        "active_users": active,
        "users": list.iter().map(db::TgUser::list_json).collect::<Vec<_>>(),
    }}))
}
