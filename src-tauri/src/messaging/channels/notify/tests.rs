//! The rules for a caller's own text (web `test/test_alerts_flow_mcp.py`,
//! "The shared rules", and the notify cases of `test/test_alerts_api.py`
//! that do not need HTTP).

use super::*;
use crate::messaging::channels::settings::{self, Change};
use crate::messaging::channels::testkit::{drain, url, with_provider, DISCORD_URL, SLACK_URL};
use crate::messaging::channels::ChannelName;
use serde_json::json;

fn urls(kind: WebhookKind) -> &'static str {
    match kind {
        WebhookKind::Slack => SLACK_URL,
        WebhookKind::Discord => DISCORD_URL,
    }
}

/// Web test/test_alerts_flow_mcp.py::test_a_long_message_is_refused_unless_shortening_is_asked_for
#[tokio::test]
async fn a_long_message_is_refused_unless_shortening_is_asked_for() {
    for kind in [WebhookKind::Slack, WebhookKind::Discord] {
        let (t, p) = with_provider();
        *p.status.lock() = 204;
        settings::save_webhook(&t.ctx, &url(kind, urls(kind))).unwrap();
        let long = json!("x".repeat(kind.max_length() + 50));
        let refused = notify(&t.ctx, kind, Some(&long), Delivery::default())
            .await
            .unwrap();
        let shortened = notify(
            &t.ctx,
            kind,
            Some(&long),
            Delivery {
                wait: false,
                shorten: true,
            },
        )
        .await
        .unwrap();
        drain(&t, kind).await;
        assert_eq!((refused.ok, refused.status_code), (false, 400));
        assert!(shortened.ok, "{:?}", shortened);
        assert_eq!(p.count(), 1);
        let text = p.text(0);
        assert_eq!(text.chars().count(), kind.max_length());
        assert!(text.ends_with("..."));
    }
}

/// Web test/test_alerts_api.py::test_notify_checks_its_input (the message
/// cases; the rest are the endpoint's).
#[test]
fn a_message_must_be_text_and_fit() {
    let kind = WebhookKind::Discord;
    for (given, says) in [
        (None, "'message' is required."),
        (Some(json!("")), "'message' is required."),
        (Some(json!("   ")), "'message' is required."),
        (Some(json!(7)), "'message' is required."),
        (
            Some(json!("x".repeat(2001))),
            "'message' must be at most 2000 characters.",
        ),
    ] {
        assert_eq!(message_problem(kind, given.as_ref()).as_deref(), Some(says));
    }
    assert_eq!(message_problem(kind, Some(&json!("x".repeat(2000)))), None);
    assert_eq!(
        message_problem(WebhookKind::Slack, Some(&json!("x".repeat(4001)))).as_deref(),
        Some("'message' must be at most 4000 characters.")
    );
}

/// Web test/test_alerts_api.py::test_notify_needs_the_channel_switched_on
#[tokio::test]
async fn a_message_needs_the_channel_switched_on() {
    let (t, p) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    settings::update(
        &t.ctx,
        ChannelName::Slack,
        Change {
            enabled: Some(false),
            categories: None,
        },
    )
    .unwrap();
    let r = notify(
        &t.ctx,
        WebhookKind::Slack,
        Some(&json!("hi")),
        Delivery::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        r,
        NotifyResult {
            ok: false,
            message: "Slack alerts are turned off. Turn them on from the Alerts page in OpenAlgo."
                .into(),
            status_code: 409,
        }
    );
    assert_eq!(p.count(), 0);
}

/// Web test/test_alerts_api.py::test_notify_without_a_usable_webhook_says_so
#[tokio::test]
async fn a_message_without_a_usable_webhook_says_so() {
    let (t, _) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL)).unwrap();
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE alert_channels SET webhook_url = 'not-a-ciphertext'",
            [],
        )
        .unwrap();
    let r = notify(
        &t.ctx,
        WebhookKind::Discord,
        Some(&json!("hi")),
        Delivery::default(),
    )
    .await
    .unwrap();
    assert_eq!(r.status_code, 409);
    assert_eq!(
        r.message,
        "No Discord webhook is saved. Add one on the Discord page in OpenAlgo."
    );
}

/// Web test/test_alerts_api.py::test_notify_waits_for_delivery_when_asked
/// and ::test_notify_reports_a_refused_delivery
#[tokio::test]
async fn waiting_reports_what_the_provider_said() {
    let (t, p) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL)).unwrap();
    *p.status.lock() = 204;
    let wait = Delivery {
        wait: true,
        shorten: false,
    };
    let sent = notify(
        &t.ctx,
        WebhookKind::Discord,
        Some(&json!("@everyone buy")),
        wait,
    )
    .await
    .unwrap();
    assert_eq!((sent.ok, sent.status_code), (true, 200));
    assert_eq!(sent.message, "Message sent to Discord.");
    assert_eq!(
        p.body(0),
        json!({"content": "@everyone buy", "allowed_mentions": {"parse": []}})
    );
    *p.status.lock() = 404;
    let refused = notify(&t.ctx, WebhookKind::Discord, Some(&json!("x")), wait)
        .await
        .unwrap();
    assert_eq!(refused.status_code, 502);
    assert!(refused.message.contains("no longer accepts this webhook"));
}

/// Web test/test_alerts_api.py::test_slack_notify_escapes_mentions
#[tokio::test]
async fn a_slack_message_shows_mentions_as_text() {
    let (t, p) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    let wait = Delivery {
        wait: true,
        shorten: false,
    };
    notify(
        &t.ctx,
        WebhookKind::Slack,
        Some(&json!("<!channel> stop")),
        wait,
    )
    .await
    .unwrap();
    assert_eq!(p.body(0), json!({"text": "&lt;!channel&gt; stop"}));
}

/// A queue that is full is a 503 for the caller who did not wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_queue_is_refused_for_now() {
    let (t, _) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    let held = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    for _ in 0..crate::messaging::channels::lane::MAX_PENDING {
        let held = held.clone();
        crate::messaging::channels::lane::submit(&t.ctx, WebhookKind::Slack, move |_| async move {
            let _ = held.acquire().await;
        });
    }
    let r = notify(
        &t.ctx,
        WebhookKind::Slack,
        Some(&json!("hi")),
        Delivery::default(),
    )
    .await
    .unwrap();
    held.add_permits(1000);
    assert_eq!(r.status_code, 503);
    assert_eq!(
        r.message,
        "Too many Slack messages are waiting to be sent. Try again shortly."
    );
}
