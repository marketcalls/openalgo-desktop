//! What `/api/v1/telegram/notify`, `/api/v1/whatsapp/notify`,
//! `/api/v1/slack/notify` and `/api/v1/discord/notify` do once the API key
//! checked out (web `restx_api/telegram_bot.py`, `whatsapp_bot.py`,
//! `webhook_alerts.py`): find the recipients, then send now or on an owned
//! task.

use super::core::Reply;
use crate::messaging::channels::notify::{self, Delivery};
use crate::messaging::channels::webhook::WebhookKind;
use crate::messaging::format::{py_str, truthy};
use crate::messaging::telegram::{db as tg_db, TelegramService};
use crate::messaging::whatsapp::{db as wa_db, normalize_phone, phone_to_jid, WhatsAppService};
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The longest WhatsApp message the web accepts.
const MAX_WHATSAPP_CHARS: usize = 4096;
/// At most this many numbers in `phones`.
const MAX_PHONES: usize = 5;

/// `/api/v1/telegram/notify`: one message to a linked Telegram user.
pub async fn telegram(ctx: &Arc<AppState>, data: &Map<String, Value>) -> Reply {
    if !TelegramService::is_bot_active(ctx) {
        tracing::info!("Telegram bot is stopped; rejecting a notify request");
        return Reply::error(
            409,
            "Telegram bot is stopped. Start the bot to send notifications.",
        );
    }
    let username = data.get("username").and_then(Value::as_str).unwrap_or("");
    let message = data.get("message").and_then(Value::as_str).unwrap_or("");
    if username.is_empty() || message.is_empty() {
        return Reply::error(400, "Username and message are required");
    }
    let user = match ctx
        .sqlite
        .conn()
        .and_then(|c| tg_db::get_user_by_username(&c, username))
    {
        Ok(Some(u)) => u,
        Ok(None) => return Reply::error(404, "User not found or not linked to Telegram"),
        Err(e) => {
            tracing::error!("Telegram notify lookup failed: {}", e);
            return Reply::error(500, "Failed to send notification");
        }
    };
    if truthy(data.get("wait_for_delivery")) {
        if ctx
            .messaging
            .telegram
            .send_alert(ctx, user.telegram_id, message)
            .await
        {
            return Reply::ok(
                json!({"status": "success", "message": "Notification sent successfully"}),
            );
        }
        return Reply::ok(
            json!({"status": "success", "message": "Notification queued for delivery"}),
        );
    }
    let weak = Arc::downgrade(ctx);
    let msg = message.to_string();
    let id = user.telegram_id;
    ctx.spawn(async move {
        if let Some(c) = weak.upgrade() {
            c.messaging.telegram.send_alert(&c, id, &msg).await;
        }
    });
    Reply::ok(json!({"status": "success", "message": "Notification queued for delivery"}))
}

/// `/api/v1/whatsapp/notify`: a text to the paired account itself, a linked
/// user, or up to five numbers.
pub async fn whatsapp(ctx: &Arc<AppState>, data: &Map<String, Value>) -> Reply {
    let svc = &ctx.messaging.whatsapp;
    if !svc.is_ready(ctx) {
        return Reply::error(
            409,
            svc.unavailable_reason().unwrap_or(
                "WhatsApp is not paired or not connected. Pair the device first from the /whatsapp page in OpenAlgo before sending.",
            ),
        );
    }
    let message = data.get("message").and_then(Value::as_str).unwrap_or("");
    if message.chars().count() > MAX_WHATSAPP_CHARS {
        return Reply::error(400, "Message must not exceed 4096 characters");
    }
    let given = |k: &str| truthy(data.get(k));
    if message.is_empty() && !given("image_path") && !given("document_path") {
        return Reply::error(
            400,
            "Provide at least one of: message, image_path, document_path",
        );
    }
    // No attachment folder on the desktop: no server-side file may be sent.
    for k in ["image_path", "document_path"] {
        if given(k) {
            return Reply::error(400, format!("{} is not allowed", k));
        }
    }
    let text = if message.is_empty() {
        data.get("caption").and_then(Value::as_str).unwrap_or("")
    } else {
        message
    };
    let targets = match whatsapp_targets(ctx, data) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let wait = data
        .get("wait_for_delivery")
        .map(|v| truthy(Some(v)))
        .unwrap_or(true);
    if wait {
        return whatsapp_send_now(ctx, &targets, text).await;
    }
    let recipients = if targets.is_empty() {
        vec![String::new()]
    } else {
        targets
    };
    let n = recipients.len();
    let weak = Arc::downgrade(ctx);
    let text = text.to_string();
    ctx.spawn(async move {
        for r in recipients {
            let Some(c) = weak.upgrade() else { return };
            let to: Vec<String> = if r.is_empty() { vec![] } else { vec![r] };
            c.messaging.whatsapp.send_alert(&c, &to, &text).await;
        }
    });
    Reply::ok(json!({
        "status": "success",
        "message": format!("Queued for {} recipient(s)", n),
        "queued": n,
    }))
}

/// Who a WhatsApp notify goes to: no JID for the paired account itself
/// (`self`, or the owner's own username), else the JIDs named.
fn whatsapp_targets(ctx: &AppState, data: &Map<String, Value>) -> Result<Vec<String>, Reply> {
    let given = |k: &str| truthy(data.get(k));
    if given("self") {
        return Ok(vec![]);
    }
    if given("phones") {
        let Some(list) = data.get("phones").and_then(Value::as_array) else {
            return Err(Reply::error(400, "'phones' must be a list"));
        };
        let t: Vec<String> = list
            .iter()
            .take(MAX_PHONES)
            .map(|p| match p {
                Value::String(_) => normalize_phone(p),
                other => normalize_phone(&Value::String(py_str(other))),
            })
            .filter(|d| !d.is_empty())
            .map(|d| phone_to_jid(&d))
            .collect();
        if t.is_empty() {
            return Err(Reply::error(400, "No valid phones in list"));
        }
        return Ok(t);
    }
    if given("phone") {
        let d = normalize_phone(data.get("phone").unwrap_or(&Value::Null));
        if d.is_empty() {
            return Err(Reply::error(400, "Invalid phone number"));
        }
        return Ok(vec![phone_to_jid(&d)]);
    }
    if !given("username") {
        return Err(Reply::error(
            400,
            "Specify one of: 'self', 'username', 'phone', or 'phones'",
        ));
    }
    let username = py_str(data.get("username").unwrap_or(&Value::Null));
    let linked = ctx
        .sqlite
        .conn()
        .and_then(|c| wa_db::get_user_by_username(&c, &username))
        .ok()
        .flatten();
    if let Some(u) = linked {
        return Ok(vec![u.whatsapp_jid]);
    }
    let owner = WhatsAppService::config(ctx)
        .owner_username
        .unwrap_or_default();
    if !owner.trim().is_empty() && owner.trim().to_lowercase() == username.trim().to_lowercase() {
        return Ok(vec![]);
    }
    Err(Reply::error(
        404,
        "Username not found or not linked to WhatsApp",
    ))
}

/// Send now and say how it went: an error when nothing was delivered and
/// something was refused, else the counts, with the report either way.
async fn whatsapp_send_now(ctx: &AppState, targets: &[String], text: &str) -> Reply {
    let report = ctx.messaging.whatsapp.send(ctx, targets, text).await;
    let delivered = report.sent.len();
    let refused = report.failed.len();
    if delivered == 0 && refused > 0 {
        let said = report.failed[0]
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("The message could not be delivered.")
            .to_string();
        return Reply::ok(json!({"status": "error", "message": said, "data": report}));
    }
    Reply::ok(json!({
        "status": "success",
        "message": format!("Delivered to {}, failed {}", delivered, refused),
        "data": report,
    }))
}

/// `/api/v1/slack/notify` and `/api/v1/discord/notify`: the caller's text
/// to the channel's saved webhook, under the channel's switch on /alerts
/// (`messaging::channels::notify`). Queued unless `wait_for_delivery` is
/// true.
pub async fn webhook(ctx: &Arc<AppState>, kind: WebhookKind, data: &Map<String, Value>) -> Reply {
    if let Some(problem) = notify::message_problem(kind, data.get("message")) {
        return Reply::error(400, problem);
    }
    let wait = match data.get("wait_for_delivery") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Reply::error(400, "'wait_for_delivery' must be true or false."),
    };
    let how = Delivery {
        wait,
        shorten: false,
    };
    match notify::notify(ctx, kind, data.get("message"), how).await {
        Ok(r) => Reply::new(
            r.status_code,
            json!({"status": if r.ok { "success" } else { "error" }, "message": r.message}),
        ),
        Err(e) => {
            tracing::error!("A {} notify could not be sent: {}", kind.label(), e);
            Reply::error(
                500,
                format!("The {} message could not be sent. Try again.", kind.label()),
            )
        }
    }
}
