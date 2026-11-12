//! What the /alerts, /slack and /discord pages read and change (web
//! `blueprints/alert_channels.py`): every channel with its status and
//! settings, a channel's switch and groups, a webhook saved or forgotten,
//! and a test message.
//!
//! Answers are the web's `{"status", "data"}` and `{"status", "message"}`
//! envelopes with its codes. A webhook URL is accepted here and never
//! returned: a channel reports only whether one is saved. The settings
//! writes, which may wait for a channel's lock, run on the blocking pool.

use super::categories::CATEGORIES;
use super::settings::{self, Change, SettingsError};
use super::{ChannelName, ChannelStatus};
use crate::services::core::Reply;
use crate::state::AppState;
use serde_json::{json, Value};
use std::sync::Arc;

const NO_SUCH_CHANNEL: &str = "There is no such alert channel.";
const WEBHOOK_ONLY: &str = "Only Slack and Discord use a webhook.";
const BUSY: &str = "Another change to this channel is still being saved. Try again.";

/// One channel as the page shows it. Never includes a webhook URL.
fn describe(ctx: &AppState, channel: ChannelName) -> Value {
    let current = settings::settings_for(ctx, channel);
    let status = super::status(ctx, channel).unwrap_or_else(|e| {
        tracing::error!(
            "Could not read the {} channel status: {}",
            channel.label(),
            e
        );
        ChannelStatus {
            configured: false,
            ready: false,
            detail: format!(
                "OpenAlgo could not check {}. Open its page for details.",
                channel.label()
            ),
        }
    });
    json!({
        "name": channel.name(),
        "label": channel.label(),
        "setup_path": channel.setup_path(),
        "uses_webhook": channel.uses_webhook(),
        "enabled": current.enabled,
        "categories": current.categories.iter().collect::<Vec<_>>(),
        "status": {
            "configured": status.configured,
            "ready": status.ready,
            "detail": status.detail,
        },
    })
}

fn described(ctx: &AppState, channel: ChannelName) -> Reply {
    Reply::ok(json!({"status": "success", "data": describe(ctx, channel)}))
}

/// `GET /alerts/channels`: every channel and every event group.
pub async fn list(ctx: &Arc<AppState>) -> Reply {
    let listed = ctx
        .blocking(|ctx| {
            Ok(json!({
                "categories": CATEGORIES.iter().map(|c| json!({
                    "id": c.id, "label": c.label, "description": c.description,
                })).collect::<Vec<_>>(),
                "channels": ChannelName::ALL.iter().map(|c| describe(ctx, *c)).collect::<Vec<_>>(),
            }))
        })
        .await;
    match listed {
        Ok(data) => Reply::ok(json!({"status": "success", "data": data})),
        Err(e) => {
            tracing::error!("Could not list the alert channels: {}", e);
            Reply::error(500, "The alert channels could not be read. Try again.")
        }
    }
}

/// A settings write's failure, as the web's route answers it.
fn refused(channel: ChannelName, e: SettingsError, failed: &str) -> Reply {
    match e {
        SettingsError::UnknownCategory(m) => Reply::error(400, m),
        SettingsError::WebhookRequired(m) => Reply::error(409, m),
        SettingsError::Busy => Reply::error(409, BUSY),
        SettingsError::Storage(e) => {
            tracing::error!(
                "Could not save the {} alert settings: {}",
                channel.label(),
                e
            );
            Reply::error(500, failed)
        }
    }
}

/// Run a settings write on the blocking pool and answer the channel.
async fn write(
    ctx: &Arc<AppState>,
    channel: ChannelName,
    failed: &'static str,
    f: impl FnOnce(&AppState) -> Result<settings::ChannelSettings, SettingsError> + Send + 'static,
) -> Reply {
    let done = ctx
        .blocking(move |ctx| Ok(f(ctx).map(|_| described(ctx, channel))))
        .await;
    match done {
        Ok(Ok(reply)) => reply,
        Ok(Err(e)) => refused(channel, e, failed),
        Err(e) => refused(channel, SettingsError::Storage(e), failed),
    }
}

/// `POST /alerts/channels/<name>`: switch a channel on or off, or choose
/// the groups it receives.
pub async fn update(ctx: &Arc<AppState>, name: &str, body: Option<Value>) -> Reply {
    let Some(channel) = ChannelName::parse(name) else {
        return Reply::error(404, NO_SUCH_CHANNEL);
    };
    let Some(Value::Object(body)) = body else {
        return Reply::error(400, "Send the settings as a JSON object.");
    };
    let enabled = match body.get("enabled") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Reply::error(400, "'enabled' must be true or false."),
    };
    let categories = match body.get("categories") {
        None | Some(Value::Null) => None,
        Some(Value::Array(list)) if list.iter().all(Value::is_string) => Some(
            list.iter()
                .filter_map(|c| c.as_str().map(String::from))
                .collect(),
        ),
        Some(_) => return Reply::error(400, "'categories' must be a list of event group names."),
    };
    let change = Change {
        enabled,
        categories,
    };
    write(
        ctx,
        channel,
        "The settings could not be saved. Try again.",
        move |ctx| settings::update(ctx, channel, change),
    )
    .await
}

/// `PUT /alerts/channels/<name>/webhook`: save a Slack or Discord webhook
/// URL. The channel is switched on.
pub async fn save_webhook(ctx: &Arc<AppState>, name: &str, body: Option<Value>) -> Reply {
    let Some(kind) = ChannelName::parse(name).and_then(ChannelName::webhook) else {
        return Reply::error(404, WEBHOOK_ONLY);
    };
    let given = body.as_ref().and_then(|b| b.get("url"));
    let url = match kind.validate_url(given) {
        Ok(url) => url,
        Err(refusal) => return Reply::error(400, refusal.0),
    };
    let reply = write(
        ctx,
        kind.channel(),
        "The webhook could not be saved. Try again.",
        move |ctx| settings::save_webhook(ctx, &url),
    )
    .await;
    if reply.status == 200 {
        tracing::info!("{} webhook saved", kind.label());
    }
    reply
}

/// `DELETE /alerts/channels/<name>/webhook`: forget a Slack or Discord
/// webhook URL. The channel is switched off.
pub async fn forget_webhook(ctx: &Arc<AppState>, name: &str) -> Reply {
    let Some(kind) = ChannelName::parse(name).and_then(ChannelName::webhook) else {
        return Reply::error(404, WEBHOOK_ONLY);
    };
    let reply = write(
        ctx,
        kind.channel(),
        "The webhook could not be removed. Try again.",
        move |ctx| settings::forget_webhook(ctx, kind),
    )
    .await;
    if reply.status == 200 {
        tracing::info!("{} webhook removed", kind.label());
    }
    reply
}

/// `POST /alerts/channels/<name>/test`: send a test message now and say
/// whether it arrived.
pub async fn send_test(ctx: &Arc<AppState>, name: &str) -> Reply {
    let Some(channel) = ChannelName::parse(name) else {
        return Reply::error(404, NO_SUCH_CHANNEL);
    };
    match super::send_test(ctx, channel).await {
        Ok(result) if result.delivered => {
            Reply::ok(json!({"status": "success", "message": result.message}))
        }
        Ok(result) => Reply::error(409, result.message),
        Err(e) => {
            tracing::error!("The {} test message failed: {}", channel.label(), e);
            Reply::error(
                500,
                format!("The {} test message could not be sent.", channel.label()),
            )
        }
    }
}
