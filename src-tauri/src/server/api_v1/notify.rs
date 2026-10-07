//! `/api/v1/telegram/notify` (web `restx_api/telegram_bot.py`) and
//! `/api/v1/whatsapp/notify` (web `restx_api/whatsapp_bot.py`).
//!
//! These two take the key from the body or the `X-API-KEY` header and
//! answer a bad key with 401 "Invalid or missing API key", as the web's
//! namespaces do (unlike the order endpoints). A valid key is enough; no
//! broker session is needed.

use super::{authorize, Auth};
use crate::messaging::telegram::{db as tg_db, TelegramService};
use crate::messaging::whatsapp::{db as wa_db, normalize_phone, phone_to_jid, WhatsAppService};
use crate::server::envelope::{error, json_response, read_json_object};
use crate::server::middleware::ClientIp;
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Map, Value};
use std::net::IpAddr;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

const BAD_KEY: &str = "Invalid or missing API key";

async fn body_and_key(
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

fn ok(body: Value) -> Response {
    json_response(StatusCode::OK, body)
}

/// POST /api/v1/telegram/notify
pub async fn telegram_notify(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let data = match body_and_key(&ctx, ip, req).await {
        Ok(d) => d,
        Err(r) => return r,
    };
    if !TelegramService::is_bot_active(&ctx) {
        tracing::info!("Telegram bot is stopped; rejecting a notify request");
        return error(
            StatusCode::CONFLICT,
            "Telegram bot is stopped. Start the bot to send notifications.",
        );
    }
    let username = data.get("username").and_then(Value::as_str).unwrap_or("");
    let message = data.get("message").and_then(Value::as_str).unwrap_or("");
    if username.is_empty() || message.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Username and message are required");
    }
    let user = match ctx
        .sqlite
        .conn()
        .and_then(|c| tg_db::get_user_by_username(&c, username))
    {
        Ok(Some(u)) => u,
        Ok(None) => {
            return error(
                StatusCode::NOT_FOUND,
                "User not found or not linked to Telegram",
            )
        }
        Err(e) => {
            tracing::error!("Telegram notify lookup failed: {}", e);
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to send notification",
            );
        }
    };
    let wait = crate::messaging::format::truthy(data.get("wait_for_delivery"));
    if wait {
        if ctx
            .messaging
            .telegram
            .send_alert(&ctx, user.telegram_id, message)
            .await
        {
            return ok(json!({"status": "success", "message": "Notification sent successfully"}));
        }
        return ok(json!({"status": "success", "message": "Notification queued for delivery"}));
    }
    let weak = Arc::downgrade(&ctx);
    let msg = message.to_string();
    let id = user.telegram_id;
    ctx.spawn(async move {
        if let Some(c) = weak.upgrade() {
            c.messaging.telegram.send_alert(&c, id, &msg).await;
        }
    });
    ok(json!({"status": "success", "message": "Notification queued for delivery"}))
}

/// POST /api/v1/whatsapp/notify
pub async fn whatsapp_notify(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let data = match body_and_key(&ctx, ip, req).await {
        Ok(d) => d,
        Err(r) => return r,
    };
    let svc = &ctx.messaging.whatsapp;
    if !svc.is_ready(&ctx) {
        return error(
            StatusCode::CONFLICT,
            svc.unavailable_reason().unwrap_or(
                "WhatsApp is not paired or not connected. Pair the device first from the /whatsapp page in OpenAlgo before sending.",
            ),
        );
    }
    let message = data.get("message").and_then(Value::as_str).unwrap_or("");
    if message.chars().count() > 4096 {
        return error(
            StatusCode::BAD_REQUEST,
            "Message must not exceed 4096 characters",
        );
    }
    let truthy = |k: &str| crate::messaging::format::truthy(data.get(k));
    if message.is_empty() && !truthy("image_path") && !truthy("document_path") {
        return error(
            StatusCode::BAD_REQUEST,
            "Provide at least one of: message, image_path, document_path",
        );
    }
    // No attachment folder on the desktop: no server-side file may be sent.
    for k in ["image_path", "document_path"] {
        if truthy(k) {
            return error(StatusCode::BAD_REQUEST, format!("{} is not allowed", k));
        }
    }
    let text = if message.is_empty() {
        data.get("caption").and_then(Value::as_str).unwrap_or("")
    } else {
        message
    };
    let targets: Vec<String> = if truthy("self") {
        vec![]
    } else if truthy("phones") {
        let Some(list) = data.get("phones").and_then(Value::as_array) else {
            return error(StatusCode::BAD_REQUEST, "'phones' must be a list");
        };
        let t: Vec<String> = list
            .iter()
            .take(5)
            .map(|p| match p {
                Value::String(_) => normalize_phone(p),
                other => normalize_phone(&Value::String(crate::messaging::format::py_str(other))),
            })
            .filter(|d| !d.is_empty())
            .map(|d| phone_to_jid(&d))
            .collect();
        if t.is_empty() {
            return error(StatusCode::BAD_REQUEST, "No valid phones in list");
        }
        t
    } else if truthy("phone") {
        let d = normalize_phone(data.get("phone").unwrap_or(&Value::Null));
        if d.is_empty() {
            return error(StatusCode::BAD_REQUEST, "Invalid phone number");
        }
        vec![phone_to_jid(&d)]
    } else if truthy("username") {
        let username = crate::messaging::format::py_str(data.get("username").unwrap_or(&Value::Null));
        let linked = ctx
            .sqlite
            .conn()
            .and_then(|c| wa_db::get_user_by_username(&c, &username))
            .ok()
            .flatten();
        match linked {
            Some(u) => vec![u.whatsapp_jid],
            None => {
                let owner = WhatsAppService::config(&ctx)
                    .owner_username
                    .unwrap_or_default();
                if !owner.trim().is_empty()
                    && owner.trim().to_lowercase() == username.trim().to_lowercase()
                {
                    vec![]
                } else {
                    return error(
                        StatusCode::NOT_FOUND,
                        "Username not found or not linked to WhatsApp",
                    );
                }
            }
        }
    } else {
        return error(
            StatusCode::BAD_REQUEST,
            "Specify one of: 'self', 'username', 'phone', or 'phones'",
        );
    };
    let wait = data
        .get("wait_for_delivery")
        .map(|v| crate::messaging::format::truthy(Some(v)))
        .unwrap_or(true);
    if wait {
        let report = svc.send(&ctx, &targets, text).await;
        let delivered = report.sent.len();
        let refused = report.failed.len();
        if delivered == 0 && refused > 0 {
            let said = report.failed[0]
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("The message could not be delivered.")
                .to_string();
            return ok(json!({"status": "error", "message": said, "data": report}));
        }
        return ok(json!({
            "status": "success",
            "message": format!("Delivered to {}, failed {}", delivered, refused),
            "data": report,
        }));
    }
    let recipients = if targets.is_empty() {
        vec![String::new()]
    } else {
        targets
    };
    let n = recipients.len();
    let weak = Arc::downgrade(&ctx);
    let text = text.to_string();
    ctx.spawn(async move {
        for r in recipients {
            let Some(c) = weak.upgrade() else { return };
            let to: Vec<String> = if r.is_empty() { vec![] } else { vec![r] };
            c.messaging.whatsapp.send_alert(&c, &to, &text).await;
        }
    });
    ok(json!({
        "status": "success",
        "message": format!("Queued for {} recipient(s)", n),
        "queued": n,
    }))
}
