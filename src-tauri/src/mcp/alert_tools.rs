//! The Slack and Discord alert tools (web `send_slack_alert`,
//! `send_discord_alert` and `_send_webhook_alert`, marketcalls/openalgo#2440).
//!
//! Each calls `/api/v1/<channel>/notify` in process with the key in the
//! body, never the URL, and returns the endpoint's answer as it is: the
//! endpoint holds every rule (switched on at /alerts, a webhook saved, the
//! length limit), so the tool adds none. A reply that is not JSON is the
//! web's structured error; a call whose answer was lost is the write advice
//! every other write tool gives (verify on the channel before retrying,
//! since the message may already be posted).

use super::dispatch::{self, transport_reply};
use super::envelope::{error, write_result, Output};
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Run `name` if it is one of the two tools; `None` for any other.
pub(super) async fn run(ctx: &Arc<AppState>, name: &str, a: &Map<String, Value>) -> Option<Output> {
    let (channel, label) = match name {
        "send_slack_alert" => ("slack", "Slack"),
        "send_discord_alert" => ("discord", "Discord"),
        _ => return None,
    };
    Some(send(ctx, channel, label, a).await.into())
}

async fn send(ctx: &Arc<AppState>, channel: &str, label: &str, a: &Map<String, Value>) -> Value {
    let action = format!("sending {} alert", label);
    let verify_with = format!(
        "the {} channel itself (the message may already have been posted)",
        label
    );
    let mut payload = Map::new();
    payload.insert(
        "message".into(),
        a.get("message").cloned().unwrap_or(Value::Null),
    );
    payload.insert(
        "wait_for_delivery".into(),
        a.get("wait_for_delivery").cloned().unwrap_or(json!(false)),
    );
    let endpoint = format!("{}/notify", channel);
    match dispatch::post_raw(ctx, &endpoint, payload).await {
        Ok(raw) => serde_json::from_str::<Value>(&raw.text).unwrap_or_else(|_| {
            error(
                format!("OpenAlgo gave no readable answer while {}.", action),
                &[
                    ("error_type", json!("http")),
                    ("status_code", json!(raw.status)),
                    ("retry_safe", json!(false)),
                    ("verify_with", json!(verify_with)),
                ],
            )
        }),
        Err(t) => write_result(transport_reply(t), &action, &verify_with),
    }
}

#[cfg(test)]
mod tests;
