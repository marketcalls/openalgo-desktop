//! The `/api/v1/telegram` bot management endpoints (web
//! `restx_api/telegram_bot.py`) behind the API key: settings, start and
//! stop, linked users, broadcast, statistics, notification preferences and
//! the webhook. `/api/v1/telegram/notify` is `notify_service`; the
//! `/telegram` page's session routes share `messaging::telegram::manage`.
//!
//! Deliberate departures from the web:
//! - The settings answer masks the token under both of its names; the web
//!   masks `bot_token` and returns the full token as `token`.
//! - A broadcast is sent (as the page's broadcast is); the web's endpoint
//!   is a stub that reports zero deliveries.
//! - A preference of the wrong type is refused with 400; the web fails
//!   with 500.
//! - A token that is not text, or the shortened form the settings answer
//!   shows, is refused instead of being saved over the real one.

use super::core::Reply;
use crate::messaging::format::{http_date, py_int};
use crate::messaging::telegram::db::{
    ConfigUpdate, UserFilter, PREFERENCE_FLAGS, PREFERENCE_TEXTS,
};
use crate::messaging::telegram::{manage, page};
use crate::state::AppState;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Longest message Telegram delivers.
const MAX_MESSAGE_CHARS: usize = 4096;

/// The first ten characters of a token, as the web shows it.
fn masked(token: &str) -> String {
    if token.chars().count() > 10 {
        format!("{}...", token.chars().take(10).collect::<String>())
    } else {
        token.to_string()
    }
}

/// `GET /api/v1/telegram/config`.
pub fn config_reply(ctx: &AppState) -> Reply {
    let read = page::bot_config(ctx).and_then(|c| Ok((c, page::bot_config_times(ctx)?)));
    match read {
        Ok((cfg, (created, updated))) => {
            let token = cfg.token.as_ref().map(|t| masked(t.expose()));
            Reply::ok(json!({"status": "success", "data": {
                "bot_token": token,
                "token": token,
                "is_active": cfg.is_active,
                "bot_username": cfg.bot_username,
                "max_message_length": cfg.max_message_length,
                "rate_limit_per_minute": cfg.rate_limit_per_minute,
                "broadcast_enabled": cfg.broadcast_enabled,
                "created_at": http_date(created.as_deref()),
                "updated_at": http_date(updated.as_deref()),
            }}))
        }
        Err(e) => {
            tracing::error!("Reading the Telegram settings failed: {}", e);
            Reply::error(500, "Failed to get bot configuration")
        }
    }
}

/// `POST /api/v1/telegram/config`: the token, broadcast switch and rate
/// limit. The web also accepts `webhook_url` and `polling_mode` and stores
/// neither; they are ignored here too.
pub fn update_config_reply(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    let mut u = ConfigUpdate::default();
    if let Some(t) = body.get("token") {
        u.token = match token_update(t) {
            Ok(token) => Some(token),
            Err(refused) => return refused,
        };
    }
    if let Some(v) = body.get("broadcast_enabled") {
        u.broadcast_enabled = Some(crate::messaging::format::truthy(Some(v)));
    }
    if let Some(v) = body.get("rate_limit_per_minute") {
        match py_int(v) {
            Some(n) if (1..=120).contains(&n) => u.rate_limit_per_minute = Some(n),
            Some(_) => return Reply::error(400, "rate_limit_per_minute must be between 1 and 120"),
            None => return Reply::error(400, "rate_limit_per_minute must be an integer"),
        }
    }
    match page::update_config(ctx, u) {
        Ok(()) => Reply::ok(json!({"status": "success", "message": "Bot configuration updated"})),
        Err(e) => {
            tracing::error!("Saving the Telegram settings failed: {}", e);
            Reply::error(500, "Failed to update bot configuration")
        }
    }
}

/// The token a settings update sets: `None` clears it (`null` or empty
/// text). A token that is not text, or the masked form `GET config` shows,
/// is refused rather than saved over the real one.
fn token_update(t: &Value) -> Result<Option<String>, Reply> {
    match t {
        Value::Null => Ok(None),
        Value::String(s) if s.trim().ends_with("...") => Err(Reply::error(
            400,
            "Send the whole bot token from BotFather, not the shortened one the settings show.",
        )),
        Value::String(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        _ => Err(Reply::error(400, "The bot token must be text.")),
    }
}

/// `POST /api/v1/telegram/start`.
pub async fn start_reply(ctx: &Arc<AppState>) -> Reply {
    match manage::start_bot(ctx).await {
        Ok(msg) => Reply::ok(json!({"status": "success", "message": msg})),
        Err((status, msg)) => Reply::error(status, msg),
    }
}

/// `POST /api/v1/telegram/stop`.
pub async fn stop_reply(ctx: &AppState) -> Reply {
    match ctx.messaging.telegram.stop(ctx).await {
        (true, msg) => Reply::ok(json!({"status": "success", "message": msg})),
        (false, msg) => Reply::error(500, msg),
    }
}

/// `GET /api/v1/telegram/users`, with the web's two optional filters.
pub fn users_reply(ctx: &AppState, filter: &UserFilter) -> Reply {
    match page::users(ctx, filter) {
        Ok(users) => {
            let list: Vec<Value> = users.iter().map(|u| u.list_json()).collect();
            Reply::ok(json!({"status": "success", "count": list.len(), "data": list}))
        }
        Err(e) => {
            tracing::error!("Reading Telegram users failed: {}", e);
            Reply::error(500, "Failed to get users")
        }
    }
}

/// `POST /api/v1/telegram/broadcast`: `message`, and optional `filters`
/// (`broker`, `notifications_enabled`) as on the page.
pub async fn broadcast_reply(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    let message = match body.get("message").and_then(Value::as_str) {
        Some(m) if !m.is_empty() => m,
        _ => return Reply::error(400, "Message is required"),
    };
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Reply::error(400, "Message must not exceed 4096 characters");
    }
    match page::bot_config(ctx) {
        Ok(cfg) if !cfg.broadcast_enabled => return Reply::error(403, "Broadcast is disabled"),
        Ok(_) => {}
        Err(e) => {
            tracing::error!("Reading the Telegram settings failed: {}", e);
            return Reply::error(500, "Failed to broadcast message");
        }
    }
    let filters = body.get("filters");
    let filter = UserFilter {
        broker: filters
            .and_then(|f| f.get("broker"))
            .and_then(Value::as_str)
            .map(String::from),
        notifications_enabled: filters
            .and_then(|f| f.get("notifications_enabled"))
            .and_then(Value::as_bool),
    };
    let (sent, failed) = manage::broadcast(ctx, message, &filter).await;
    Reply::ok(json!({
        "status": "success",
        "message": format!("Broadcast sent to {} users, failed for {} users", sent, failed),
        "success_count": sent,
        "fail_count": failed,
    }))
}

/// `GET /api/v1/telegram/stats?days=`: 1 to 365 days, 7 when absent or
/// not a number.
pub fn stats_reply(ctx: &AppState, days: Option<&str>) -> Reply {
    let days = days
        .and_then(|d| d.trim().parse::<i64>().ok())
        .map_or(7, |d| d.clamp(1, 365));
    match page::command_stats(ctx, days) {
        Ok(s) => {
            let by_type: Map<String, Value> = s
                .commands_by_type
                .iter()
                .map(|(c, n)| (c.clone(), json!(n)))
                .collect();
            Reply::ok(json!({"status": "success", "data": {
                "total_commands": s.total_commands,
                "commands_by_type": by_type,
                "active_users": s.active_users,
                "top_users": s.top_users.iter().map(|(u, n)| json!([u, n])).collect::<Vec<_>>(),
                "period_days": s.period_days,
            }}))
        }
        Err(e) => {
            tracing::error!("Reading Telegram command stats failed: {}", e);
            Reply::error(500, "Failed to get statistics")
        }
    }
}

/// `GET /api/v1/telegram/preferences?telegram_id=`.
pub fn preferences_reply(ctx: &AppState, telegram_id: Option<i64>) -> Reply {
    let Some(id) = telegram_id.filter(|id| *id != 0) else {
        return Reply::error(400, "telegram_id is required");
    };
    match page::preferences(ctx, id) {
        Ok(p) => Reply::ok(json!({"status": "success", "data": p})),
        Err(e) => {
            tracing::error!("Reading Telegram preferences failed: {}", e);
            Reply::error(500, "Failed to get preferences")
        }
    }
}

/// `POST /api/v1/telegram/preferences`: `telegram_id` and any of the
/// preferences, the yes/no ones as booleans and the others as text.
pub fn update_preferences_reply(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    let Some(id) = body
        .get("telegram_id")
        .and_then(py_int)
        .filter(|id| *id != 0)
    else {
        return Reply::error(400, "telegram_id is required");
    };
    let mut prefs: Vec<(&'static str, Value)> = Vec::new();
    for name in PREFERENCE_FLAGS {
        match body.get(name) {
            None => {}
            Some(v @ Value::Bool(_)) => prefs.push((name, v.clone())),
            Some(_) => return Reply::error(400, format!("{name} must be true or false")),
        }
    }
    for name in PREFERENCE_TEXTS {
        match body.get(name) {
            None => {}
            Some(v @ Value::String(_)) => prefs.push((name, v.clone())),
            Some(_) => return Reply::error(400, format!("{name} must be text")),
        }
    }
    match page::save_preferences(ctx, id, &prefs) {
        Ok(()) => {
            Reply::ok(json!({"status": "success", "message": "Preferences updated successfully"}))
        }
        Err(e) => {
            tracing::error!("Saving Telegram preferences failed: {}", e);
            Reply::error(500, "Failed to update preferences")
        }
    }
}

/// The secret Telegram echoes in `X-Telegram-Bot-Api-Secret-Token`: the
/// first 32 hex characters of the token's SHA-256, as the web derives it
/// when `TELEGRAM_WEBHOOK_SECRET` is not set (the desktop reads no
/// environment). `None` without a token.
fn webhook_secret(ctx: &AppState) -> Option<String> {
    let cfg = page::bot_config(ctx).ok()?;
    let token = cfg.token?;
    let digest = Sha256::digest(token.expose().as_bytes());
    Some(hex::encode(digest)[..32].to_string())
}

/// `POST /api/v1/telegram/webhook`: the status and plain-text body. As on
/// the web, a verified update is acknowledged and not processed (the bot
/// polls).
pub fn webhook_answer(
    ctx: &AppState,
    secret_header: Option<&str>,
    update: Option<&Value>,
) -> (u16, &'static str) {
    if let Some(expected) = webhook_secret(ctx) {
        use subtle::ConstantTimeEq;
        match secret_header {
            None => return (401, "Unauthorized"),
            Some(got) if !bool::from(got.as_bytes().ct_eq(expected.as_bytes())) => {
                return (403, "Forbidden")
            }
            Some(_) => {}
        }
    }
    match update {
        None | Some(Value::Null) => (200, ""),
        Some(Value::Object(m)) if m.is_empty() => (200, ""),
        Some(Value::Object(m)) if m.contains_key("update_id") => (200, ""),
        _ => (400, "Bad Request"),
    }
}
