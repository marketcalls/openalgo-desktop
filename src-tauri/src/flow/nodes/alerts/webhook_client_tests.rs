//! The real client's Slack and Discord paths (web
//! `test/test_alerts_flow_mcp.py`, "Flow"): the notify rules of
//! `/api/v1/<channel>/notify`, queued, a long message shortened.

use crate::flow::contract::client::app::AppFlowClient;
use crate::flow::contract::client::FlowClient;
use crate::messaging::channels::settings::{self, Change};
use crate::messaging::channels::testkit::{drain, url, with_provider, DISCORD_URL, SLACK_URL};
use crate::messaging::channels::webhook::WebhookKind;
use crate::security::Secret;
use crate::state::testing::TestCtx;
use serde_json::{json, Value};
use std::sync::Arc;

const KINDS: [WebhookKind; 2] = [WebhookKind::Slack, WebhookKind::Discord];

fn client(t: &TestCtx) -> AppFlowClient {
    AppFlowClient::new(Arc::downgrade(&t.ctx), Secret::new("k"))
}

fn saved(kind: WebhookKind) -> &'static str {
    match kind {
        WebhookKind::Slack => SLACK_URL,
        WebhookKind::Discord => DISCORD_URL,
    }
}

async fn send(t: &TestCtx, kind: WebhookKind, message: Value) -> Value {
    match kind {
        WebhookKind::Slack => client(t).slack(&message).await,
        WebhookKind::Discord => client(t).discord(&message).await,
    }
}

/// Web ::test_the_flow_node_posts_to_the_saved_webhook
#[tokio::test]
async fn the_node_posts_to_the_saved_webhook() {
    for kind in KINDS {
        let (t, p) = with_provider();
        *p.status.lock() = 204;
        settings::save_webhook(&t.ctx, &url(kind, saved(kind))).unwrap();
        let result = send(&t, kind, json!("Bought NIFTY")).await;
        drain(&t, kind).await;
        assert_eq!(
            result,
            json!({"status": "success", "data": {"message": "Notification queued."}})
        );
        assert_eq!(p.count(), 1);
        assert_eq!(p.url(0), saved(kind));
        assert_eq!(p.text(0), "Bought NIFTY");
    }
}

/// Web ::test_a_long_flow_message_is_shortened_not_lost
#[tokio::test]
async fn a_long_message_is_shortened_not_lost() {
    for kind in KINDS {
        let (t, p) = with_provider();
        *p.status.lock() = 204;
        settings::save_webhook(&t.ctx, &url(kind, saved(kind))).unwrap();
        let long = "y".repeat(kind.max_length() * 2);
        let result = send(&t, kind, json!(long)).await;
        drain(&t, kind).await;
        assert_eq!(result["status"], "success", "{}", result);
        assert_eq!(p.text(0).chars().count(), kind.max_length());
    }
}

/// Web ::test_the_flow_node_posts_nothing_while_the_channel_is_off and
/// ::test_the_flow_node_posts_nothing_without_a_webhook
#[tokio::test]
async fn nothing_is_posted_while_off_or_without_a_webhook() {
    for kind in KINDS {
        let (t, p) = with_provider();
        let none = send(&t, kind, json!("Bought NIFTY")).await;
        assert_eq!(none["status"], "error");
        settings::save_webhook(&t.ctx, &url(kind, saved(kind))).unwrap();
        settings::update(
            &t.ctx,
            kind.channel(),
            Change {
                enabled: Some(false),
                categories: None,
            },
        )
        .unwrap();
        let off = send(&t, kind, json!("Bought NIFTY")).await;
        assert_eq!(off["status"], "error");
        assert!(
            off["error"].as_str().unwrap().contains("turned off"),
            "{}",
            off
        );
        assert_eq!(p.count(), 0);
    }
}

/// Web ::test_a_flow_alert_that_fails_unexpectedly_says_where_to_look:
/// a saved webhook that cannot be read is a sentence naming the log, never
/// the database's own words.
#[tokio::test]
async fn an_unexpected_failure_says_where_to_look() {
    let (t, _) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    assert!(settings::settings_for(&t.ctx, WebhookKind::Slack.channel()).enabled);
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute_batch("ALTER TABLE alert_channels RENAME TO alert_channels_away")
        .unwrap();
    let result = send(&t, WebhookKind::Slack, json!("Bought NIFTY")).await;
    assert_eq!(
        result,
        json!({"status": "error",
            "error": "The Slack alert could not be sent. The OpenAlgo log has the details."})
    );
}

/// The web passes the interpolated value as it is: one that is not text is
/// refused by the notify rules, not posted as its printed form.
#[tokio::test]
async fn a_message_that_is_not_text_is_refused() {
    let (t, p) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL)).unwrap();
    let result = send(&t, WebhookKind::Discord, json!(42)).await;
    assert_eq!(
        result,
        json!({"status": "error", "error": "'message' is required."})
    );
    assert_eq!(p.count(), 0);
}
