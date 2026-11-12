//! Post a caller's own text to a webhook alert channel (web
//! `services/alerts/notify.py`).
//!
//! The one set of rules for text a trader writes, wherever it is sent from:
//! `/api/v1/slack/notify` and `/api/v1/discord/notify`, the Flow Slack and
//! Discord nodes, and the MCP tools (which call the endpoints). Keeping
//! them here is what stops a Flow node posting a message the REST call
//! would refuse, or the other way round. A message posts only while the
//! channel is switched on at /alerts and has a usable webhook saved, the
//! rule automated order alerts follow.

use super::settings;
use super::webhook::{self, WebhookKind};
use crate::state::AppState;
use serde_json::Value;
use std::sync::Arc;

/// What happened to one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyResult {
    /// Posted, or queued to be posted.
    pub ok: bool,
    /// A sentence for the trader.
    pub message: String,
    /// The HTTP status the REST endpoint answers with.
    pub status_code: u16,
}

impl NotifyResult {
    fn new(ok: bool, message: impl Into<String>, status_code: u16) -> Self {
        Self {
            ok,
            message: message.into(),
            status_code,
        }
    }
}

/// Why `message` cannot be posted to `kind`, or `None` when it can.
pub fn message_problem(kind: WebhookKind, message: Option<&Value>) -> Option<String> {
    let Some(text) = message
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
    else {
        return Some("'message' is required.".into());
    };
    let max = kind.max_length();
    (text.chars().count() > max).then(|| format!("'message' must be at most {} characters.", max))
}

/// How a notify call wants its message handled.
#[derive(Debug, Clone, Copy, Default)]
pub struct Delivery {
    /// Post now and report whether the provider took it, instead of
    /// queueing it on the channel's lane.
    pub wait: bool,
    /// Cut a message over the channel's limit to fit, instead of refusing
    /// it: for callers nobody is watching, such as a running Flow, where a
    /// refused alert is simply lost.
    pub shorten: bool,
}

/// Post `message` to the channel's saved webhook, or say why not. A
/// settings table that cannot be read gives the channel's defaults, under
/// which Slack and Discord are off, so the answer is "turned off". An
/// error is a saved webhook that could not be read.
pub async fn notify(
    ctx: &Arc<AppState>,
    kind: WebhookKind,
    message: Option<&Value>,
    how: Delivery,
) -> crate::error::Result<NotifyResult> {
    let shortened = match message.and_then(Value::as_str) {
        Some(text) if how.shorten && text.chars().count() > kind.max_length() => {
            Some(Value::String(kind.fit(text)))
        }
        _ => None,
    };
    let message = shortened.as_ref().or(message);
    if let Some(problem) = message_problem(kind, message) {
        return Ok(NotifyResult::new(false, problem, 400));
    }
    let text = message.and_then(Value::as_str).unwrap_or_default();
    let label = kind.label();
    if !settings::settings_for(ctx, kind.channel()).enabled {
        return Ok(NotifyResult::new(
            false,
            format!(
                "{} alerts are turned off. Turn them on from the Alerts page in OpenAlgo.",
                label
            ),
            409,
        ));
    }
    let result = webhook::send_text(ctx, kind, text, how.wait).await?;
    if result.delivered {
        return Ok(NotifyResult::new(true, result.message, 200));
    }
    if webhook::saved_url(ctx, kind)?.is_none() {
        return Ok(NotifyResult::new(false, result.message, 409));
    }
    let code = if how.wait { 502 } else { 503 };
    Ok(NotifyResult::new(false, result.message, code))
}

#[cfg(test)]
mod tests;
