//! The subscribe and unsubscribe acknowledgements tell what the broker side
//! did (audit C01, C02), through the real feed server against a scripted
//! `FakeSource`. Ports of web test/test_websocket_unsubscribe_contract.py
//! at a1b9e3250 (the registry-level halves are in
//! `feed::registry::lifecycle::tests`).

use crate::feed_support;

use feed_support::*;
use openalgo_desktop_lib::feed::{FakeSource, InstrumentKey, MarketUpdate, Mode};
use serde_json::{json, Value};
use std::time::Duration;

fn reliance() -> InstrumentKey {
    InstrumentKey::new("RELIANCE", "NSE")
}

async fn subscribed(url: &str, mode: &str) -> Client {
    let mut c = Client::connect(url).await;
    c.auth().await;
    let ack = c
        .request(json!({"action": "subscribe", "mode": mode,
            "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}]}))
        .await;
    assert_eq!(ack["status"], "success", "{}", ack);
    c
}

fn unsubscribe(mode: &str, rid: &str) -> Value {
    json!({"action": "unsubscribe", "mode": mode, "request_id": rid,
        "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}]})
}

/// The next `market_data` frame, skipping acks.
async fn next_tick(c: &mut Client) -> Option<Value> {
    loop {
        let v = c.recv_within(Duration::from_secs(2)).await?;
        if v["type"] == "market_data" {
            return Some(v);
        }
    }
}

/// Web test_last_subscriber_failure_preserves_both_registries_and_retry_success_removes_them:
/// a refused release answers `error` with the item under its canonical
/// mode and the request id; the client still owns the subscription (it
/// keeps streaming); the retry succeeds and releases it once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_release_is_an_error_item_and_the_retry_releases_it() {
    let h = start_default().await;
    let mut c = subscribed(&h.url, "LTP").await;
    h.source.fail_release(
        reliance(),
        Mode::Ltp,
        "The broker refused to stop streaming RELIANCE.",
    );

    let ack = c.request(unsubscribe("LTP", "req-7")).await;
    assert_eq!(
        ack,
        json!({
            "type": "unsubscribe",
            "status": "error",
            "message": "Unsubscription processing complete",
            "successful": [],
            "failed": [{
                "symbol": "RELIANCE",
                "exchange": "NSE",
                "mode": "LTP",
                "status": "error",
                "message": "The broker refused to stop streaming RELIANCE.",
                "broker": BROKER,
            }],
            "broker": BROKER,
            "request_id": "req-7",
        })
    );
    assert_eq!(h.source.active_count(), 1);
    h.source
        .publish(MarketUpdate::ltp(reliance(), 1424.0, 1_791_000_000_000));
    let tick = next_tick(&mut c).await.expect("the owner still streams");
    assert_eq!(tick["data"]["ltp"], 1424.0);

    let ack = c.request(unsubscribe("LTP", "req-8")).await;
    assert_eq!(ack["status"], "success", "{}", ack);
    assert_eq!(ack["successful"][0]["mode"], "LTP");
    assert_eq!(ack["request_id"], "req-8");
    assert_eq!(h.source.active_count(), 0);
    assert!(
        h.source.violations().is_empty(),
        "{:?}",
        h.source.violations()
    );
    h.handle.stop().await;
}

/// Web test_invalid_or_exceptional_broker_response_preserves_exact_ownership:
/// a release the broker never answers is answered as not done once the
/// wait is over, and the subscription stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_release_keeps_the_subscription() {
    let h = start_with(FakeSource::new(known()), |c| {
        c.op_timeout = Duration::from_millis(300)
    })
    .await;
    let mut c = subscribed(&h.url, "Quote").await;
    h.source.hold_release(reliance(), Mode::Quote);

    let ack = c.request(unsubscribe("Quote", "q")).await;
    assert_eq!(ack["status"], "error", "{}", ack);
    assert_eq!(ack["failed"][0]["mode"], "Quote");
    assert!(
        ack["failed"][0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("stays subscribed")),
        "{}",
        ack
    );
    assert_eq!(h.source.active_count(), 1);
    let ack = c.request(unsubscribe("Quote", "q2")).await;
    assert_eq!(ack["status"], "success", "{}", ack);
    assert_eq!(h.source.active_count(), 0);
    h.handle.stop().await;
}

/// Web test_multi_client_unsubscribe_removes_only_the_caller_until_the_last_ack:
/// while another client holds the key, an unsubscribe succeeds locally and
/// the broker is not asked; the last owner's release is the broker's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_release_at_the_broker_only_with_the_last() {
    let h = start_default().await;
    let mut a = subscribed(&h.url, "Depth").await;
    let mut b = subscribed(&h.url, "Depth").await;
    h.source
        .fail_release(reliance(), Mode::Depth, "still streaming");

    let ack = a.request(unsubscribe("Depth", "a")).await;
    assert_eq!(ack["status"], "success", "{}", ack);
    assert_eq!(ack["successful"][0]["mode"], "Depth");
    assert_eq!(h.source.active_count(), 1);

    let ack = b.request(unsubscribe("Depth", "b")).await;
    assert_eq!(ack["status"], "error", "{}", ack);
    let ack = b.request(unsubscribe("Depth", "b2")).await;
    assert_eq!(ack["status"], "success", "{}", ack);
    assert_eq!(h.source.active_count(), 0);
    h.handle.stop().await;
}

/// Mixed modes in one request: the refused mode is a failed item and the
/// other a successful one, so the request is `partial`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_mode_makes_the_request_partial() {
    let h = start_default().await;
    let mut c = subscribed(&h.url, "LTP").await;
    let ack = c
        .request(json!({"action": "subscribe", "mode": "Quote",
            "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}]}))
        .await;
    assert_eq!(ack["status"], "success", "{}", ack);
    h.source
        .fail_release(reliance(), Mode::Ltp, "ltp still live");

    let ack = c
        .request(json!({"action": "unsubscribe", "symbols": [
            {"symbol": "RELIANCE", "exchange": "NSE", "mode": "LTP"},
            {"symbol": "RELIANCE", "exchange": "NSE", "mode": 2}]}))
        .await;
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert_eq!(ack["failed"][0]["mode"], "LTP");
    assert_eq!(ack["failed"][0]["message"], "ltp still live");
    assert_eq!(ack["successful"][0]["mode"], "Quote");
    assert_eq!(h.source.active(), vec![(reliance(), Mode::Ltp, 5)]);
    h.handle.stop().await;
}

/// Web test_unsubscribe_all_failure_preserves_ownership_until_retry_succeeds:
/// `unsubscribe_all` lists what the broker would not release as failed and
/// keeps it; asked again, it releases it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_all_keeps_and_lists_what_the_broker_refused() {
    let h = start_default().await;
    let mut c = subscribed(&h.url, "LTP").await;
    let ack = c
        .request(json!({"action": "subscribe", "mode": "Quote",
            "symbols": [{"symbol": "SBIN", "exchange": "NSE"}]}))
        .await;
    assert_eq!(ack["status"], "success", "{}", ack);
    h.source.fail_release(reliance(), Mode::Ltp, "still live");

    let ack = c
        .request(json!({"action": "unsubscribe_all", "request_id": "all-7"}))
        .await;
    assert_eq!(ack["type"], "unsubscribe");
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert_eq!(ack["request_id"], "all-7");
    assert_eq!(
        (
            ack["failed"][0]["symbol"].clone(),
            ack["failed"][0]["mode"].clone()
        ),
        (json!("RELIANCE"), json!("LTP"))
    );
    assert_eq!(ack["successful"][0]["symbol"], "SBIN");
    assert_eq!(h.source.active(), vec![(reliance(), Mode::Ltp, 5)]);

    let ack = c
        .request(json!({"action": "unsubscribe_all", "request_id": "all-8"}))
        .await;
    assert_eq!(ack["status"], "success", "{}", ack);
    assert_eq!(ack["successful"][0]["mode"], "LTP");
    assert_eq!(h.source.active_count(), 0);
    h.handle.stop().await;
}

/// Web test_explicit_refusal_remains_owned_until_the_real_disconnect_tears_it_down_once:
/// a disconnect is its own path. The owner a refused release kept goes
/// with the socket, the source is asked once more, and nothing of the
/// client stays in the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disconnect_ends_the_kept_ownership() {
    let h = start_default().await;
    let mut c = subscribed(&h.url, "LTP").await;
    h.source
        .fail_release(reliance(), Mode::Ltp, "explicit refusal");
    let ack = c.request(unsubscribe("LTP", "x")).await;
    assert_eq!(ack["status"], "error", "{}", ack);
    drop(c);
    assert!(
        eventually(Duration::from_secs(5), || h.handle.stats().clients == 0).await,
        "the client goes with its socket"
    );
    let stats = h.handle.stats();
    assert_eq!((stats.client_subscriptions, stats.source_keys), (0, 0));
    assert_eq!(h.source.active_count(), 0, "released at the disconnect");
    assert!(
        h.source.violations().is_empty(),
        "{:?}",
        h.source.violations()
    );
    h.handle.stop().await;
}

/// Audit P-12, as the web rebuilds a disconnected adapter when a client
/// authenticates (`websocket_proxy/server.py` 905-931): each feed client
/// that signs in asks the source to resume a feed the broker ended for a
/// reason the trader can clear, so closing the other app and reconnecting
/// brings prices back without a logout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_signing_in_resumes_the_feed() {
    let h = start_default().await;
    assert_eq!(h.source.resumes(), 0);
    let mut a = Client::connect(&h.url).await;
    a.auth().await;
    assert_eq!(h.source.resumes(), 1);
    let mut b = Client::connect(&h.url).await;
    b.auth().await;
    assert_eq!(h.source.resumes(), 2);
    h.handle.stop().await;
}
