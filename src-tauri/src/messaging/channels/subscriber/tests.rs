//! Order events reach the channels routed to them, on the app's real bus
//! (web `test/test_alerts_routing.py`, "On a real event bus").

use super::*;
use crate::events::{GttKind, Mode, OrderMeta};
use crate::messaging::channels::settings::{self, Change};
use crate::messaging::channels::testkit::{url, with_provider, DISCORD_URL, SLACK_URL};
use crate::messaging::channels::webhook::WebhookKind;
use serde_json::json;
use std::time::Duration;

fn meta(api_type: &str, request: serde_json::Value, response: serde_json::Value) -> OrderMeta {
    OrderMeta {
        mode: Mode::Live,
        api_type: api_type.into(),
        request_data: request,
        response_data: response,
    }
}

fn placed() -> Event {
    Event::OrderPlaced {
        meta: meta(
            "placeorder",
            json!({"symbol": "SBIN", "action": "BUY", "quantity": "1"}),
            json!({"status": "success", "orderid": "1"}),
        ),
        strategy: String::new(),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        quantity: 1,
        pricetype: "MARKET".into(),
        product: "MIS".into(),
        orderid: "1".into(),
    }
}

fn failed() -> Event {
    Event::OrderFailed {
        meta: meta(
            "placeorder",
            json!({"symbol": "SBIN"}),
            json!({"status": "error", "message": "Rejected"}),
        ),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        error_message: "Rejected".into(),
    }
}

fn gtt_triggered() -> Event {
    crate::sandbox::events::gtt_triggered("G1", "SBIN", "NSE", "", &[790.0], "77")
}

async fn wait_for(count: impl Fn() -> usize, n: usize) {
    for _ in 0..500 {
        if count() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("waited for {} posts, saw {}", n, count());
}

/// Web test/test_alerts_routing.py::test_an_order_event_costs_the_bus_two_alert_callbacks,
/// in the desktop's form: one subscriber per channel, each on the alert
/// topics only.
#[tokio::test]
async fn every_channel_is_its_own_subscriber() {
    let (t, _) = with_provider();
    let names: Vec<&str> = t.ctx.bus.queue_depths().iter().map(|q| q.name).collect();
    for channel in ChannelName::ALL {
        assert_eq!(
            names.iter().filter(|n| **n == channel.name()).count(),
            1,
            "{}",
            channel.name()
        );
    }
    let alerts = ChannelAlerts::new(std::sync::Weak::new(), ChannelName::Slack);
    assert_eq!(alerts.topics(), categories::alert_topics());
}

/// Web test/test_alerts_routing.py::test_one_order_event_reaches_every_channel_exactly_once
/// and ::test_a_failure_event_reaches_no_channel
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_order_event_reaches_each_channel_once_and_a_failure_none() {
    let (t, p) = with_provider();
    *p.status.lock() = 204;
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL)).unwrap();
    t.ctx.bus.publish(failed());
    t.ctx.bus.publish(placed());
    wait_for(|| p.count(), 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(p.count(), 2, "one post per channel");
    let mut hosts: Vec<String> = (0..2).map(|i| p.url(i)).collect();
    hosts.sort();
    assert!(hosts[0].starts_with("https://discord.com/"));
    assert!(hosts[1].starts_with("https://hooks.slack.com/"));
    for i in 0..2 {
        assert!(p.text(i).contains("Order Placed"), "{}", p.text(i));
    }
}

/// Web test/test_alerts_routing.py::test_a_group_routed_away_from_a_channel_is_not_handed_to_it
/// and ::test_no_alert_is_published_when_no_channel_wants_it
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_groups_routed_to_a_channel_reach_it() {
    let (t, p) = with_provider();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    settings::update(
        &t.ctx,
        ChannelName::Slack,
        Change {
            enabled: None,
            categories: Some(vec!["gtt_orders".into()]),
        },
    )
    .unwrap();
    t.ctx.bus.publish(placed());
    t.ctx.bus.publish(gtt_triggered());
    wait_for(|| p.count(), 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(p.count(), 1);
    let text = p.text(0);
    assert!(text.starts_with("*GTT Triggered*"), "{}", text);
    assert!(text.contains("Trigger Prices: 790.0") && text.contains("Order ID: 77"));
    // Discord was never set up: nothing reached it.
    assert!(p.url(0).starts_with("https://hooks.slack.com/"));
}

/// Web test/test_alerts_routing.py::test_a_failing_channel_does_not_stop_the_others:
/// Slack's saved webhook can no longer be opened; Discord still posts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_channel_does_not_stop_the_others() {
    let (t, p) = with_provider();
    *p.status.lock() = 204;
    settings::save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    settings::save_webhook(&t.ctx, &url(WebhookKind::Discord, DISCORD_URL)).unwrap();
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE alert_channels SET webhook_url = 'not-a-ciphertext' WHERE channel = 'slack'",
            [],
        )
        .unwrap();
    t.ctx.bus.publish(Event::Gtt {
        kind: GttKind::Placed,
        meta: meta(
            "placegttorder",
            json!({"symbol": "SBIN", "trigger_type": "SINGLE", "triggerprice_tg": 850.0}),
            json!({"status": "success", "trigger_id": "G9"}),
        ),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        trigger_id: "G9".into(),
        triggered_order_id: String::new(),
    });
    wait_for(|| p.count(), 1).await;
    assert!(p.url(0).starts_with("https://discord.com/"));
    assert!(p.text(0).contains("Trigger ID: G9"));
}
