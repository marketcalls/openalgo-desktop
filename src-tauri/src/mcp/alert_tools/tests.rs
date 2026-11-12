//! The MCP tools call the real notify endpoint (web
//! `test/test_alerts_flow_mcp.py`, "MCP").

use super::*;
use crate::messaging::channels::settings::{self, Change};
use crate::messaging::channels::testkit::{drain, url, with_provider, DISCORD_URL, SLACK_URL};
use crate::messaging::channels::webhook::WebhookKind;
use crate::state::testing::TestCtx;

fn signed_up() -> (TestCtx, Arc<crate::messaging::channels::testkit::Provider>) {
    let (t, p) = with_provider();
    crate::services::auth_service::AuthService::setup(
        &t.ctx,
        "trader",
        "trader@example.com",
        "Secret@123",
    )
    .unwrap();
    (t, p)
}

fn args(message: &str, wait: bool) -> Map<String, Value> {
    let mut a = Map::new();
    a.insert("message".into(), json!(message));
    a.insert("wait_for_delivery".into(), json!(wait));
    a
}

async fn answer(t: &TestCtx, tool: &str, a: &Map<String, Value>) -> Value {
    match run(&t.ctx, tool, a).await {
        Some(Output::Json(v)) => v,
        other => panic!("{:?}", other),
    }
}

/// Web ::test_the_mcp_tool_posts_through_the_endpoint
#[tokio::test]
async fn the_tool_posts_through_the_endpoint() {
    for (tool, kind, saved) in [
        ("send_slack_alert", WebhookKind::Slack, SLACK_URL),
        ("send_discord_alert", WebhookKind::Discord, DISCORD_URL),
    ] {
        let (t, p) = signed_up();
        *p.status.lock() = 204;
        settings::save_webhook(&t.ctx, &url(kind, saved)).unwrap();
        let v = answer(&t, tool, &args("Breakout on NIFTY", true)).await;
        assert_eq!(v["status"], "success", "{}", v);
        assert_eq!(p.count(), 1);
        assert_eq!(p.url(0), saved);
        assert_eq!(p.text(0), "Breakout on NIFTY");
        // Queued by default.
        let queued = answer(&t, tool, &args("again", false)).await;
        assert_eq!(
            queued,
            json!({"status": "success", "message": "Notification queued."})
        );
        drain(&t, kind).await;
        assert_eq!(p.count(), 2);
    }
}

/// Web ::test_the_mcp_tool_is_refused_while_the_channel_is_off
#[tokio::test]
async fn the_tool_is_refused_while_the_channel_is_off() {
    let (t, p) = signed_up();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL)).unwrap();
    settings::update(
        &t.ctx,
        WebhookKind::Discord.channel(),
        Change {
            enabled: Some(false),
            categories: None,
        },
    )
    .unwrap();
    let v = answer(&t, "send_discord_alert", &args("Breakout on NIFTY", false)).await;
    assert_eq!(v["status"], "error");
    assert!(
        v["message"].as_str().unwrap().contains("turned off"),
        "{}",
        v
    );
    assert_eq!(p.count(), 0);
}

/// Another tool's name is not these tools' to run.
#[tokio::test]
async fn other_tools_are_left_alone() {
    let (t, _) = with_provider();
    assert!(run(&t.ctx, "send_telegram_alert", &Map::new())
        .await
        .is_none());
}
