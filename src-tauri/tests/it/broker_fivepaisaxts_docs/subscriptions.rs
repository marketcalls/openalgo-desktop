//! 5paisa XTS keeps its subscriptions whole (web 5749d4126,
//! marketcalls/openalgo#2504): one entry per instrument however often a
//! reconnect replays it, and a refused batch sent again one instrument at
//! a time. Ported from web test/test_fivepaisaxts_xts_docs.py.

use super::fake::serve;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use openalgo_desktop_lib::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
};
use openalgo_desktop_lib::brokers::families::xts::streaming::{
    run_command, Command, CommandReport, FeedSource, SubscriptionTarget, XtsFeed,
};
use openalgo_desktop_lib::brokers::fivepaisaxts::CONFIG;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// The feed's book
// ---------------------------------------------------------------------------

fn sub(token: &str) -> FeedSubscription {
    FeedSubscription {
        symbol: format!("S{token}"),
        exchange: "NSE".into(),
        token: token.into(),
        brsymbol: format!("S{token}"),
        brexchange: "NSECM".into(),
        mode: FeedMode::Quote,
        depth: 5,
    }
}

fn ticks_for(feed: &mut XtsFeed, packet: &str) -> Vec<String> {
    let frame = format!("42[\"1105-json-partial\",\"{packet}\"]");
    feed.parse(&Message::Text(frame))
        .into_iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some(t.symbol),
            _ => None,
        })
        .collect()
}

/// Web `test_replaying_the_book_on_every_reconnect_does_not_grow_it`,
/// `test_unsubscribe_removes_the_instrument_from_the_book` and
/// `test_1105_ticks_pass_only_for_confirmed_instruments`: the feed keeps one
/// entry per instrument however often it is subscribed again, one
/// unsubscribe removes it, and a 1105 tick passes only for an instrument
/// it holds.
#[tokio::test]
async fn the_book_keeps_one_entry_per_instrument() {
    // A closed loopback port: the REST subscriptions fail and are retried
    // in the feed's own task, which goes with the feed.
    let mut feed = XtsFeed::new(
        &CONFIG,
        openalgo_desktop_lib::brokers::common::http::client(),
        "http://127.0.0.1:1".into(),
        None,
        FeedSource::stored("tok", "U1"),
    );
    for _ in 0..20 {
        feed.subscribe_frames(&[sub("1140025"), sub("2885")]);
    }
    feed.unsubscribe_frames(&[sub("2885")]);
    assert_eq!(
        ticks_for(&mut feed, "t:1_1140025,110:2067.75"),
        ["S1140025"]
    );
    assert!(ticks_for(&mut feed, "t:1_2885,110:2501.5").is_empty());
    assert!(ticks_for(&mut feed, "t:1_999,110:1.0").is_empty());
}

// ---------------------------------------------------------------------------
// Subscriptions: a refused batch goes again one instrument at a time
// ---------------------------------------------------------------------------

const DUPLICATE: &str =
    "{\"type\":\"error\",\"code\":\"e-session-0002\",\"description\":\"Instrument Already Subscribed !\"}";
const REFUSED: &str =
    "{\"type\":\"error\",\"code\":\"e-x\",\"description\":\"Invalid instrument\"}";

/// A subscription endpoint that answers a request of several instruments
/// with `batch`, and one instrument by `single(id)`; it records each
/// request's instrument ids.
async fn endpoint(
    batch: (u16, &'static str),
    single: fn(i64) -> (u16, &'static str),
) -> (String, Arc<Mutex<Vec<Vec<i64>>>>) {
    let seen: Arc<Mutex<Vec<Vec<i64>>>> = Arc::default();
    let s2 = seen.clone();
    let app = Router::new().route(
        "/sub",
        post(move |Json(b): Json<Value>| {
            let seen = s2.clone();
            async move {
                let ids: Vec<i64> = b["instruments"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|i| i["exchangeInstrumentID"].as_i64())
                            .collect()
                    })
                    .unwrap_or_default();
                seen.lock().push(ids.clone());
                let ok = "{\"type\":\"success\",\"result\":{\"listQuotes\":[]}}";
                let (status, body) = match ids.as_slice() {
                    [one] => single(*one),
                    [] => (200, ok),
                    _ => batch,
                };
                (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body)
            }
        }),
    );
    let url = serve(app).await;
    (format!("{url}/sub"), seen)
}

fn subscribe(ids: &[i64]) -> Command {
    Command {
        subscribe: true,
        code: 1501,
        instruments: ids
            .iter()
            .map(|i| json!({"exchangeSegment": 1, "exchangeInstrumentID": i}))
            .collect(),
        waited: None,
        then_release: None,
    }
}

async fn run(url: &str, cmd: &Command) -> CommandReport {
    let http = openalgo_desktop_lib::brokers::common::http::client();
    let target = SubscriptionTarget {
        broker: CONFIG.id,
        http: &http,
        url,
        token: "tok",
        gap: Duration::ZERO,
        split: CONFIG.hooks.split_refused_batch,
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    run_command(&target, cmd, &tx, &mut true).await
}

fn refused_ids(report: &CommandReport) -> Vec<i64> {
    report
        .refused
        .iter()
        .filter_map(|(i, _)| i["exchangeInstrumentID"].as_i64())
        .collect()
}

/// Web `test_a_refused_batch_is_sent_one_at_a_time`: one instrument the
/// server will not take no longer costs the others; "already subscribed"
/// counts as subscribed.
#[tokio::test]
async fn a_refused_batch_is_sent_one_at_a_time() {
    let (url, seen) = endpoint((400, DUPLICATE), |id| match id {
        1 => (200, "{\"type\":\"success\",\"result\":{\"listQuotes\":[]}}"),
        2 => (400, DUPLICATE),
        _ => (400, REFUSED),
    })
    .await;
    let report = run(&url, &subscribe(&[1, 2, 3])).await;
    assert_eq!(*seen.lock(), [vec![1, 2, 3], vec![1], vec![2], vec![3]]);
    assert_eq!(refused_ids(&report), [3]);

    // Refused for any other reason: the same.
    let (url, seen) = endpoint((400, REFUSED), |id| match id {
        2 => (400, REFUSED),
        _ => (200, "{\"type\":\"success\",\"result\":{\"listQuotes\":[]}}"),
    })
    .await;
    let report = run(&url, &subscribe(&[1, 2, 3])).await;
    assert_eq!(*seen.lock(), [vec![1, 2, 3], vec![1], vec![2], vec![3]]);
    assert_eq!(refused_ids(&report), [2]);
}

/// Web `test_a_single_already_subscribed_instrument_counts_as_subscribed`.
#[tokio::test]
async fn a_single_already_subscribed_instrument_counts_as_subscribed() {
    let (url, seen) = endpoint((400, DUPLICATE), |_| (400, DUPLICATE)).await;
    let report = run(&url, &subscribe(&[7])).await;
    assert_eq!(*seen.lock(), [vec![7]]);
    assert!(report.refused.is_empty() && report.retry.is_empty());
}

/// Web `test_a_network_error_reaches_the_adapter_so_it_queues_the_batch_again`:
/// a call with no answer goes back to be sent again (BF-03), never taken
/// for subscribed.
#[tokio::test]
async fn a_failed_call_is_sent_again() {
    let report = run("http://127.0.0.1:1/sub", &subscribe(&[1])).await;
    assert_eq!(report.retry.len(), 1);
    assert!(report.refused.is_empty());
}
