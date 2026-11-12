//! Telegram and WhatsApp as alert channels (web
//! `services/alerts/channels/telegram.py` and `whatsapp.py`).
//!
//! Their own rules are unchanged and still decided by their services: a
//! Telegram alert goes only while the bot is running and only to linked
//! accounts with notifications on; a WhatsApp alert goes to the paired
//! device's owner while it is paired and connected. This adds what the
//! /alerts page shows (how each stands, a test message) and WhatsApp's
//! text from the shared formatter.

use super::formatting::{AlertMessage, SEPARATOR};
use super::{ChannelStatus, DeliveryResult, TEST_MESSAGE};
use crate::messaging::telegram::{self, db::TgUser, db::UserFilter};
use crate::messaging::whatsapp::WhatsAppService;
use crate::state::AppState;
use std::sync::Arc;

/// An alert as WhatsApp text: bold title and banner, then the lines (the
/// web's `WhatsAppAlertService.render`).
pub fn render_whatsapp(message: &AlertMessage) -> String {
    let mut parts = vec![format!("*{}*", message.title)];
    if let Some(strategy) = &message.strategy {
        parts.push(format!("Strategy: {}", strategy));
    }
    if let Some(banner) = message.banner {
        parts.push(format!("*{}*", banner));
        parts.push(SEPARATOR.to_string());
    }
    parts.extend(message.lines.iter().cloned());
    parts.join("\n")
}

/// Linked Telegram accounts that receive alerts.
fn telegram_recipients(ctx: &AppState) -> crate::error::Result<Vec<TgUser>> {
    let all = telegram::page::users(ctx, &UserFilter::default())?;
    Ok(all
        .into_iter()
        .filter(|u| u.notifications_enabled)
        .collect())
}

/// How Telegram stands.
pub fn telegram_status(ctx: &AppState) -> crate::error::Result<ChannelStatus> {
    let config = telegram::page::bot_config(ctx)?;
    let status = |configured, ready, detail: &str| ChannelStatus {
        configured,
        ready,
        detail: detail.to_string(),
    };
    if config.token.is_none() {
        return Ok(status(
            false,
            false,
            "No bot token is saved. Add one on the Telegram page.",
        ));
    }
    if !config.is_active {
        return Ok(status(
            true,
            false,
            "The bot is stopped. Start it on the Telegram page to receive alerts.",
        ));
    }
    let recipients = telegram_recipients(ctx)?.len();
    if recipients == 0 {
        return Ok(status(
            true,
            false,
            "The bot is running, but no linked account has notifications on. Send /link to the bot from Telegram.",
        ));
    }
    let accounts = if recipients == 1 {
        "account receives"
    } else {
        "accounts receive"
    };
    Ok(status(
        true,
        true,
        &format!(
            "The bot is running. {} linked {} alerts.",
            recipients, accounts
        ),
    ))
}

/// Send Telegram's test message to every linked account that receives
/// alerts.
pub async fn telegram_test(ctx: &Arc<AppState>) -> crate::error::Result<DeliveryResult> {
    let state = telegram_status(ctx)?;
    if !state.ready {
        return Ok(DeliveryResult::failed(state.detail));
    }
    let mut sent = 0;
    for user in telegram_recipients(ctx)? {
        if ctx
            .messaging
            .telegram
            .send_alert(ctx, user.telegram_id, TEST_MESSAGE)
            .await
        {
            sent += 1;
        }
    }
    if sent == 0 {
        return Ok(DeliveryResult::failed(
            "Telegram did not accept the test message. Check that the bot token on the Telegram page is still valid.",
        ));
    }
    Ok(DeliveryResult::sent("Test message sent to Telegram."))
}

/// How WhatsApp stands.
pub fn whatsapp_status(ctx: &AppState) -> ChannelStatus {
    let service = &ctx.messaging.whatsapp;
    if !WhatsAppService::is_paired(ctx) {
        return ChannelStatus {
            configured: false,
            ready: false,
            detail: "No device is paired. Pair one on the WhatsApp page.".into(),
        };
    }
    if !service.is_ready(ctx) {
        return ChannelStatus {
            configured: true,
            ready: false,
            detail: service
                .unavailable_reason()
                .unwrap_or("The paired device is not connected. Open the WhatsApp page.")
                .into(),
        };
    }
    ChannelStatus {
        configured: true,
        ready: true,
        detail: "Paired and connected. Alerts go to the paired number.".into(),
    }
}

/// Send WhatsApp's test message to the paired number, where order alerts
/// go.
pub async fn whatsapp_test(ctx: &Arc<AppState>) -> crate::error::Result<DeliveryResult> {
    let state = whatsapp_status(ctx);
    if !state.ready {
        return Ok(DeliveryResult::failed(state.detail));
    }
    if ctx
        .messaging
        .whatsapp
        .send_alert(ctx, &[], TEST_MESSAGE)
        .await
    {
        return Ok(DeliveryResult::sent("Test message sent to WhatsApp."));
    }
    Ok(DeliveryResult::failed(
        "WhatsApp did not accept the test message. Open the WhatsApp page and check that the device is still connected.",
    ))
}
