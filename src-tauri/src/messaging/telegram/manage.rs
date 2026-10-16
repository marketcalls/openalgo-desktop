//! Bot operations shared by the `/telegram` page's session routes
//! (`server::routes::telegram`) and the `/api/v1/telegram` management
//! endpoints (`rest`): starting the bot, and a broadcast to linked users.

use super::db::UserFilter;
use super::page;
use crate::state::AppState;
use std::sync::Arc;

/// Why the settings could not be read, as the page says it.
pub const SETTINGS_UNREADABLE: &str = "Could not read the Telegram settings. Try again.";

/// Web `initialize_bot_sync` then `start_bot`: check the saved token with
/// Telegram, then start polling. A refusal carries the web's status and a
/// trader-facing message.
pub async fn start_bot(ctx: &Arc<AppState>) -> Result<String, (u16, String)> {
    let cfg = page::bot_config(ctx).map_err(|e| {
        tracing::error!("Reading the Telegram settings failed: {}", e);
        (500, SETTINGS_UNREADABLE.to_string())
    })?;
    let Some(token) = cfg.token else {
        return Err((400, "Bot token not configured".to_string()));
    };
    let svc = &ctx.messaging.telegram;
    let (good, msg) = svc.initialize(ctx, &token).await;
    if !good {
        return Err((500, msg));
    }
    match svc.start(ctx).await {
        (true, msg) => Ok(msg),
        (false, msg) => Err((500, msg)),
    }
}

/// Send `message` to every linked user that passes `filter` and has
/// notifications on. Returns `(sent, failed)`.
pub async fn broadcast(ctx: &AppState, message: &str, filter: &UserFilter) -> (usize, usize) {
    let (mut sent, mut failed) = (0, 0);
    let users = page::users(ctx, filter).unwrap_or_else(|e| {
        tracing::error!("Reading Telegram users for a broadcast failed: {}", e);
        vec![]
    });
    for u in users.iter().filter(|u| u.notifications_enabled) {
        if ctx
            .messaging
            .telegram
            .send_alert(ctx, u.telegram_id, message)
            .await
        {
            sent += 1;
        } else {
            failed += 1;
        }
    }
    (sent, failed)
}
