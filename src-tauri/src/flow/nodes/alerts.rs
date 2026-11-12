//! Telegram Alert and WhatsApp Alert (the web's `execute_telegram_alert`
//! and `execute_whatsapp_alert`, `flow_executor_service.py` 3059-3078), and
//! Slack Alert and Discord Alert (`execute_slack_alert`,
//! `execute_discord_alert`, marketcalls/openalgo#2440).
//!
//! The node interpolates its text, hands it to the client and logs the
//! client's answer; the client sends through the desktop's native Telegram
//! and WhatsApp services with the web's checks and sentences
//! (`contract::client::app`). The recipient of a Telegram alert is always
//! the trader's own linked account: a legacy `username` on the node is
//! ignored. Slack and Discord post to the webhook saved on /slack or
//! /discord under the `/api/v1/<channel>/notify` rules
//! (`messaging::channels::notify`), queued, a message over the channel's
//! limit shortened. A failed send is the node's failure, so the branch
//! below it stops, as on the web.

use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome, ERROR, INFO};
use crate::flow::contract::pyfmt;
use async_trait::async_trait;
use serde_json::{json, Map, Value};

fn level_of(result: &Value) -> &'static str {
    if result.get("status").and_then(Value::as_str) == Some("success") {
        INFO
    } else {
        ERROR
    }
}

/// A field interpolated once (`context.interpolate(node_data.get(key, ""))`).
fn text_field(ctx: &NodeCtx, data: &Map<String, Value>, key: &str) -> Value {
    ctx.vars
        .interpolate_value(data.get(key).unwrap_or(&json!("")))
}

pub struct TelegramAlert;

#[async_trait]
impl FlowNode for TelegramAlert {
    fn node_type(&self) -> &'static str {
        "telegramAlert"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let message = pyfmt::str(&text_field(ctx, data, "message"));
        ctx.info(format!("Sending Telegram alert: {}", message));
        let result = ctx.client.telegram(&message).await;
        ctx.log(
            format!("Telegram result: {}", pyfmt::str(&result)),
            level_of(&result),
        );
        NodeOutcome::result(result)
    }
}

pub struct WhatsappAlert;

#[async_trait]
impl FlowNode for WhatsappAlert {
    fn node_type(&self) -> &'static str {
        "whatsappAlert"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let message = pyfmt::str(&text_field(ctx, data, "message"));
        let to = text_field(ctx, data, "to");
        // Empty after interpolation: the paired device's own number.
        let to = pyfmt::truthy(&to).then(|| pyfmt::str(&to));
        ctx.info(format!(
            "Sending WhatsApp alert to {}: {}",
            to.as_deref().unwrap_or("self"),
            message
        ));
        let result = ctx.client.whatsapp(&message, to.as_deref()).await;
        ctx.log(
            format!("WhatsApp result: {}", pyfmt::str(&result)),
            level_of(&result),
        );
        NodeOutcome::result(result)
    }
}

/// The run log keeps a preview, not the message: one variable can make a
/// message as long as the channel allows, and every run's log is stored.
pub const LOG_PREVIEW_CHARS: usize = 200;

/// The message as the run log shows it (the web's preview).
fn preview(text: &str) -> String {
    let length = text.chars().count();
    if length <= LOG_PREVIEW_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(LOG_PREVIEW_CHARS).collect();
    format!("{}... ({} characters)", head, length)
}

/// Which webhook channel a node posts to.
#[derive(Clone, Copy)]
enum Webhook {
    Slack,
    Discord,
}

/// The web's `_execute_webhook_alert`: the message interpolated once,
/// a preview logged, the client's answer logged and returned.
async fn webhook_alert(which: Webhook, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
    let label = match which {
        Webhook::Slack => "Slack",
        Webhook::Discord => "Discord",
    };
    let message = text_field(ctx, data, "message");
    ctx.info(format!(
        "Sending {} alert: {}",
        label,
        preview(&pyfmt::str(&message))
    ));
    let result = match which {
        Webhook::Slack => ctx.client.slack(&message).await,
        Webhook::Discord => ctx.client.discord(&message).await,
    };
    ctx.log(
        format!("{} result: {}", label, pyfmt::str(&result)),
        level_of(&result),
    );
    NodeOutcome::result(result)
}

pub struct SlackAlert;

#[async_trait]
impl FlowNode for SlackAlert {
    fn node_type(&self) -> &'static str {
        "slackAlert"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        webhook_alert(Webhook::Slack, ctx, data).await
    }
}

pub struct DiscordAlert;

#[async_trait]
impl FlowNode for DiscordAlert {
    fn node_type(&self) -> &'static str {
        "discordAlert"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        webhook_alert(Webhook::Discord, ctx, data).await
    }
}

#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod webhook_client_tests;
