//! Slack and Discord webhooks: which URLs are accepted, and what gets
//! posted (web `test/test_alerts_webhooks.py`, and the log redaction case
//! of `test/test_alerts_api.py`).

use super::*;
use crate::events::{Mode, OrderMeta};
use crate::messaging::channels::formatting::{AlertMessage, LIVE_BANNER};
use crate::messaging::channels::testkit::{
    drain, url, with_provider, DISCORD_URL, SECRETS, SLACK_URL,
};
use crate::messaging::channels::{lane, settings, store, ChannelName};
use std::sync::Arc;
use tokio::sync::Semaphore;

fn check(kind: WebhookKind, url: &str) -> Result<String, InvalidWebhookUrl> {
    kind.validate_url(Some(&Value::String(url.into())))
        .map(|u| u.expose().to_string())
}

// ------------------------------------------------ Which URLs are accepted

/// Web test/test_alerts_webhooks.py::test_slack_accepts_its_own_webhooks
#[test]
fn slack_accepts_its_own_webhooks() {
    for u in [
        SLACK_URL.to_string(),
        format!("{}/", SLACK_URL),
        format!("  {}  ", SLACK_URL),
        "https://hooks.slack.com:443/services/T1/B2/xyz".into(),
        "https://HOOKS.SLACK.COM/services/T1/B2/xyz".into(),
    ] {
        assert_eq!(check(WebhookKind::Slack, &u).unwrap(), u.trim(), "{}", u);
    }
}

/// Web test/test_alerts_webhooks.py::test_discord_accepts_its_own_webhooks
#[test]
fn discord_accepts_its_own_webhooks() {
    for u in [
        DISCORD_URL,
        "https://discordapp.com/api/webhooks/1/token_-1",
        "https://discord.com/api/v10/webhooks/1/token",
        "https://ptb.discord.com/api/webhooks/1/token",
        "https://canary.discord.com/api/webhooks/1/token",
        "https://discord.com/api/webhooks/1/token?thread_id=987654321",
    ] {
        assert_eq!(check(WebhookKind::Discord, u).unwrap(), u, "{}", u);
    }
}

/// Web test/test_alerts_webhooks.py::test_slack_refuses_anything_else
#[test]
fn slack_refuses_anything_else() {
    let long = format!("https://hooks.slack.com/services/T1/B2/{}", "x".repeat(600));
    for given in [
        Value::Null,
        Value::String("".into()),
        Value::String("   ".into()),
        serde_json::json!(42),
    ] {
        assert!(WebhookKind::Slack.validate_url(Some(&given)).is_err());
    }
    assert!(WebhookKind::Slack.validate_url(None).is_err());
    for u in [
        "http://hooks.slack.com/services/T1/B2/xyz",
        "https://hooks.slack.com.example.net/services/T1/B2/xyz",
        "https://example.net/hooks.slack.com/services/T1/B2/xyz",
        "https://evil@hooks.slack.com/services/T1/B2/xyz",
        "https://user:pw@hooks.slack.com/services/T1/B2/xyz",
        "https://hooks.slack.com:8443/services/T1/B2/xyz",
        "https://hooks.slack.com:+443/services/T1/B2/xyz",
        "https://hooks.slack.com/services/T1/B2/xyz#frag",
        "https://hooks.slack.com/services/T1/B2/xyz?redirect=1",
        "https://hooks.slack.com/triggers/T1/B2/xyz",
        "https://hooks.slack.com/services/T1/B2/../../../admin",
        "https://hooks.slack.com/services/T1/B2/xyz\\@127.0.0.1",
        "https://hooks.slack.com/services/T1/B2/x yz",
        "\u{1}https://hooks.slack.com/services/T1/B2/xyz",
        "https://hooks.slack.com/services/T1/B2/x\u{7f}yz",
        "https://127.0.0.1/services/T1/B2/xyz",
        &long,
        DISCORD_URL,
    ] {
        assert!(check(WebhookKind::Slack, u).is_err(), "{:?}", u);
    }
}

/// Web test/test_alerts_webhooks.py::test_discord_refuses_anything_else
#[test]
fn discord_refuses_anything_else() {
    for u in [
        "https://discord.com.example.net/api/webhooks/1/token",
        "https://evil-discord.com/api/webhooks/1/token",
        "https://discord.com/api/webhooks/notanumber/token",
        "https://discord.com/api/webhooks/1/token?thread_id=1&wait=true",
        "https://discord.com/api/webhooks/1/token?thread_id=abc",
        "https://discord.com/api/channels/1/messages",
        "https://cdn.discordapp.com/api/webhooks/1/token",
        "https://discord.gg/api/webhooks/1/token",
        SLACK_URL,
    ] {
        assert!(check(WebhookKind::Discord, u).is_err(), "{:?}", u);
    }
}

/// Web test/test_alerts_webhooks.py::test_the_refusal_says_what_a_webhook_url_looks_like
#[test]
fn the_refusal_says_what_a_webhook_url_looks_like() {
    let refused = check(WebhookKind::Slack, "https://example.com/hook").unwrap_err();
    assert_eq!(
        refused.0,
        "That is not a Slack webhook URL. It should begin with https://hooks.slack.com/services/"
    );
    assert_eq!(
        check(WebhookKind::Discord, " ").unwrap_err().0,
        "Paste the Discord webhook URL."
    );
}

/// A validated URL never prints.
#[test]
fn a_webhook_url_never_prints() {
    let u = url(WebhookKind::Slack, SLACK_URL);
    assert!(!format!("{:?}", u).contains(SECRETS[0]));
}

// ------------------------------------------------------- What gets posted

fn message() -> AlertMessage {
    AlertMessage {
        title: "Order Placed",
        banner: Some(LIVE_BANNER),
        strategy: Some("mean_reversion <!channel>".into()),
        lines: vec![
            "Symbol: SBIN".into(),
            "Error: Rejected <!here> & *bold* @everyone".into(),
            "Time: 09:15:30".into(),
        ],
    }
}

/// Web test/test_alerts_webhooks.py::test_slack_escapes_every_value_and_bolds_the_title
#[test]
fn slack_escapes_every_value_and_bolds_the_title() {
    let rendered = WebhookKind::Slack.render(&message());
    assert_eq!(rendered.lines().next(), Some("*Order Placed*"));
    assert!(!rendered.contains("<!channel>") && !rendered.contains("<!here>"));
    assert!(rendered.contains("&lt;!here&gt; &amp; *bold*"));
    assert!(!rendered.contains('<') && !rendered.contains('>'));
}

/// Web test/test_alerts_webhooks.py::test_slack_escapes_text_an_api_caller_sends
#[test]
fn slack_escapes_text_an_api_caller_sends() {
    assert_eq!(
        WebhookKind::Slack.render_text("<!channel> a & b"),
        "&lt;!channel&gt; a &amp; b"
    );
}

/// Web test/test_alerts_webhooks.py::test_discord_shows_values_literally_and_bolds_the_title
#[test]
fn discord_shows_values_literally_and_bolds_the_title() {
    let rendered = WebhookKind::Discord.render(&message());
    assert_eq!(rendered.lines().next(), Some("**Order Placed**"));
    assert!(rendered.contains("Strategy: mean\\_reversion"));
    assert!(rendered.contains("\\*bold\\*"));
}

/// Web test/test_alerts_webhooks.py::test_nothing_posted_to_discord_can_mention_anyone
#[test]
fn nothing_posted_to_discord_can_mention_anyone() {
    let payload = WebhookKind::Discord.payload("@everyone look");
    assert_eq!(
        payload["allowed_mentions"],
        serde_json::json!({"parse": []})
    );
    assert_eq!(
        WebhookKind::Discord.render_text("@everyone **x**"),
        "@everyone **x**"
    );
}

/// Web test/test_alerts_webhooks.py::test_an_alert_too_long_for_discord_is_cut_to_fit
#[test]
fn an_alert_too_long_for_discord_is_cut_to_fit() {
    let long = WebhookKind::Discord.fit(&"x".repeat(5000));
    assert_eq!(long.chars().count(), 2000);
    assert!(long.ends_with("..."));
    let short = WebhookKind::Slack.fit("short");
    assert_eq!(short, "short");
}

// ---------------------------- Posting, and what a failure tells the trader

/// Web test/test_alerts_webhooks.py::test_slack_takes_a_200
#[tokio::test]
async fn slack_takes_a_200() {
    let (t, p) = with_provider();
    let result = post(&t.ctx, &url(WebhookKind::Slack, SLACK_URL), "hello").await;
    assert!(result.delivered, "{:?}", result);
    assert_eq!(result.message, "Message sent to Slack.");
    assert_eq!(p.url(0), SLACK_URL);
    assert_eq!(
        p.requests.lock()[0].body.as_deref(),
        Some(&b"{\"text\":\"hello\"}"[..])
    );
    assert_eq!(p.requests.lock()[0].connect_to.port(), 443);
}

/// Web test/test_alerts_webhooks.py::test_discord_takes_a_204
#[tokio::test]
async fn discord_takes_a_204() {
    let (t, p) = with_provider();
    *p.status.lock() = 204;
    let result = post(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL), "hello").await;
    assert!(result.delivered);
}

/// Web test/test_alerts_webhooks.py::test_a_refusal_names_the_cause_and_the_next_step
#[tokio::test]
async fn a_refusal_names_the_cause_and_the_next_step() {
    let (t, p) = with_provider();
    for (status, says) in [
        (404, "no longer accepts this webhook"),
        (403, "no longer accepts this webhook"),
        (410, "no longer accepts this webhook"),
        (429, "Try again in a minute"),
        (500, "having trouble"),
        (400, "refused the message"),
        // A redirect is not followed: it is a refusal of this host's.
        (302, "refused the message"),
    ] {
        *p.status.lock() = status;
        let result = post(&t.ctx, &url(WebhookKind::Slack, SLACK_URL), "hello").await;
        assert!(!result.delivered);
        assert!(
            result.message.contains(says),
            "{}: {}",
            status,
            result.message
        );
        assert!(!result.message.contains(&status.to_string()));
    }
    assert_eq!(p.count(), 7, "one request each, never a second");
}

/// Web test/test_alerts_webhooks.py::test_a_network_failure_is_explained
#[tokio::test]
async fn a_network_failure_is_explained() {
    let (t, p) = with_provider();
    for (fail, says) in [
        (
            crate::flow::nodes::http::client::TIMED_OUT,
            "did not answer in time",
        ),
        ("could not connect (refused)", "could not reach Slack"),
    ] {
        *p.fail.lock() = Some(fail.into());
        let result = post(&t.ctx, &url(WebhookKind::Slack, SLACK_URL), "hello").await;
        assert!(
            !result.delivered && result.message.contains(says),
            "{:?}",
            result
        );
    }
}

/// Desktop: a provider's name that resolves into this computer or the home
/// network is never posted to (Flow's httpRequest guard), and a name that
/// does not resolve is a connection problem.
#[tokio::test]
async fn a_provider_name_resolving_to_a_private_address_is_never_posted_to() {
    let (t, p) = with_provider();
    for private in ["127.0.0.1", "192.168.1.10", "10.1.2.3", "::1", "100.64.0.1"] {
        *p.addresses.lock() = vec![private.parse().unwrap()];
        let result = post(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL), "hi").await;
        assert!(!result.delivered);
        assert!(
            result.message.contains("private network"),
            "{}",
            result.message
        );
    }
    *p.addresses.lock() = vec!["104.18.0.10".parse().unwrap(), "10.0.0.1".parse().unwrap()];
    assert!(
        !post(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL), "hi")
            .await
            .delivered
    );
    p.addresses.lock().clear();
    let result = post(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL), "hi").await;
    assert!(result.message.contains("could not reach Discord"));
    assert_eq!(p.count(), 0);
}

/// Web test/test_alerts_webhooks.py::test_the_url_never_reaches_a_log_line_or_a_reply
#[test]
fn the_url_never_reaches_a_log_line_or_a_reply() {
    let (replies, logged) = crate::services::error_log::kept_while(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (t, p) = with_provider();
            let mut replies = Vec::new();
            for status in [404, 500] {
                *p.status.lock() = status;
                replies.push(
                    post(&t.ctx, &url(WebhookKind::Slack, SLACK_URL), "hello")
                        .await
                        .message,
                );
            }
            *p.fail.lock() = Some(format!("cannot connect to {}", SLACK_URL));
            replies.push(
                post(&t.ctx, &url(WebhookKind::Slack, SLACK_URL), "hello")
                    .await
                    .message,
            );
            replies
        })
    });
    let written: Vec<&String> = replies.iter().chain(logged.iter()).collect();
    assert!(logged.len() >= 3, "{:?}", logged);
    for line in written {
        for secret in SECRETS {
            assert!(!line.contains(secret), "{}", line);
        }
    }
}

/// Web test/test_alerts_api.py::test_a_webhook_url_is_redacted_from_logs
#[test]
fn a_webhook_url_is_redacted_from_logs() {
    for u in [
        SLACK_URL,
        DISCORD_URL,
        "https://discordapp.com/api/v10/webhooks/1/secret-token",
        "https://canary.discord.com/api/webhooks/1/secret-token?thread_id=5",
        "https://hooks.slack.com:443/services/T1/B2/secret-token",
        "https://discord.com:443/api/webhooks/1/secret-token",
    ] {
        let line = redact_urls(&format!("POST {} failed", u));
        assert!(line.contains("<redacted>"), "{}", line);
        let token = u.rsplit('/').next().unwrap().split('?').next().unwrap();
        assert!(!line.contains(token), "{}", line);
    }
    assert_eq!(redact_urls("HTTP status code=200"), "HTTP status code=200");
}

// --------------------------------------------------------- The saved URL

/// Web test/test_alerts_webhooks.py::test_a_stored_url_that_is_not_the_channels_is_never_posted_to
#[tokio::test]
async fn a_stored_url_that_is_not_the_channels_is_never_posted_to() {
    let (t, p) = with_provider();
    store::save_webhook(
        &t.ctx.sqlite.conn().unwrap(),
        &t.ctx.security,
        "slack",
        "http://127.0.0.1:5000/admin",
        true,
        &[],
        t.ctx.now(),
    )
    .unwrap();
    let result = send_test(&t.ctx, WebhookKind::Slack).await.unwrap();
    assert!(!result.delivered);
    assert_eq!(p.count(), 0);
}

/// Web test/test_alerts_webhooks.py::test_a_test_without_a_webhook_asks_for_one
#[tokio::test]
async fn a_test_without_a_webhook_asks_for_one() {
    let (t, p) = with_provider();
    let result = send_test(&t.ctx, WebhookKind::Slack).await.unwrap();
    assert_eq!(
        result.message,
        "Save a Slack webhook first, then send a test."
    );
    assert_eq!(p.count(), 0);
}

/// Web test/test_alerts_webhooks.py::test_an_unusable_saved_webhook_reads_as_not_set_up
#[tokio::test]
async fn an_unusable_saved_webhook_reads_as_not_set_up() {
    let (t, _) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE alert_channels SET webhook_url = 'not-a-ciphertext'",
            [],
        )
        .unwrap();
    let state = status(&t.ctx, WebhookKind::Slack).unwrap();
    assert!(!state.configured && !state.ready);
    assert!(state.detail.contains("Save it again"), "{}", state.detail);
}

// ---------------------------- Order alerts are queued, the queue is bounded

/// Web test/test_alerts_webhooks.py::test_an_order_alert_is_posted_in_the_background
#[tokio::test]
async fn an_order_alert_is_posted_in_the_background() {
    let (t, p) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL)).unwrap();
    *p.status.lock() = 204;
    let meta = OrderMeta {
        mode: Mode::Live,
        api_type: "placeorder".into(),
        request_data: serde_json::json!({"symbol": "SBIN", "action": "BUY"}),
        response_data: serde_json::json!({"status": "success", "orderid": "42"}),
    };
    deliver(&t.ctx, WebhookKind::Discord, &meta);
    drain(&t, WebhookKind::Discord).await;
    let body = p.body(0);
    let text = body["content"].as_str().unwrap();
    assert!(
        text.contains("**Order Placed**") && text.contains("Order ID: 42"),
        "{}",
        text
    );
    assert_eq!(body["allowed_mentions"], serde_json::json!({"parse": []}));
}

/// Hold both of a lane's workers until `gate` gets permits.
fn occupy(t: &crate::state::testing::TestCtx, kind: WebhookKind, gate: &Arc<Semaphore>, n: usize) {
    for _ in 0..n {
        let gate = gate.clone();
        assert!(lane::submit(&t.ctx, kind, move |_| async move {
            let _ = gate.acquire().await;
        }));
    }
}

/// Web test/test_alerts_webhooks.py::test_a_full_lane_drops_rather_than_grows
#[tokio::test]
async fn a_full_lane_drops_rather_than_grows() {
    use futures_util::FutureExt;
    let (t, _) = with_provider();
    let probe = lane::probe(&t.ctx, 1);
    let gate = Arc::new(Semaphore::new(0));
    let held = gate.clone();
    let first = probe.submit(Box::new(move |_| {
        async move {
            let _ = held.acquire().await;
        }
        .boxed()
    }));
    let second = probe.submit(Box::new(|_| async {}.boxed()));
    gate.add_permits(10);
    assert!(first);
    assert!(!second);
}

/// Web test/test_alerts_webhooks.py::test_a_stalled_provider_does_not_hold_up_the_other
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_provider_does_not_hold_up_the_other() {
    let (t, p) = with_provider();
    let stalled = Arc::new(Semaphore::new(0));
    let slack = t
        .ctx
        .messaging
        .channels
        .lanes
        .get(&t.ctx, WebhookKind::Slack);
    occupy(&t, WebhookKind::Slack, &stalled, slack.capacity());
    assert!(
        !lane::submit(&t.ctx, WebhookKind::Slack, |_| async {}),
        "Slack is full"
    );
    settings::save_webhook(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL)).unwrap();
    *p.status.lock() = 204;
    assert!(
        send_text(&t.ctx, WebhookKind::Discord, "hi", false)
            .await
            .unwrap()
            .delivered
    );
    drain(&t, WebhookKind::Discord).await;
    assert_eq!(p.count(), 1);
    assert!(p.url(0).starts_with("https://discord.com/"));
    stalled.add_permits(1000);
}

/// Web test/test_alerts_webhooks.py::test_a_queued_post_is_never_sent_to_a_webhook_removed_meanwhile
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_post_is_never_sent_to_a_webhook_removed_meanwhile() {
    let (t, p) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    let held = Arc::new(Semaphore::new(0));
    occupy(&t, WebhookKind::Slack, &held, lane::WORKERS_PER_CHANNEL);
    assert!(
        send_text(&t.ctx, WebhookKind::Slack, "hi", false)
            .await
            .unwrap()
            .delivered
    );
    settings::forget_webhook(&t.ctx, WebhookKind::Slack).unwrap();
    held.add_permits(10);
    drain(&t, WebhookKind::Slack).await;
    assert_eq!(p.count(), 0);
}

/// Web test/test_alerts_webhooks.py::test_a_queued_post_is_never_sent_once_the_channel_is_switched_off
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_post_is_never_sent_once_the_channel_is_switched_off() {
    let (t, p) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    let held = Arc::new(Semaphore::new(0));
    occupy(&t, WebhookKind::Slack, &held, lane::WORKERS_PER_CHANNEL);
    assert!(
        send_text(&t.ctx, WebhookKind::Slack, "hi", false)
            .await
            .unwrap()
            .delivered
    );
    settings::update(
        &t.ctx,
        ChannelName::Slack,
        settings::Change {
            enabled: Some(false),
            categories: None,
        },
    )
    .unwrap();
    held.add_permits(10);
    drain(&t, WebhookKind::Slack).await;
    assert_eq!(p.count(), 0);
}
