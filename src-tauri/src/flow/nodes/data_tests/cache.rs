//! The history cache (N-dataCommon-11, N-dataCommon-12; web
//! `indicator_service.fetch_history_cached`): one call for identical
//! requests, shared by concurrent callers, kept 30 seconds, only when it has
//! rows, never more than its bound; and the nodes that use it.

use super::support::{ctx, logs, run};
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::client::{ApiCall, FlowClient, History};
use crate::flow::nodes::history::{
    api_interval, shared_cache, HistoryCache, HISTORY_CACHE_MAXSIZE, HISTORY_CACHE_TTL,
    HISTORY_FLIGHT_TIMEOUT,
};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// A client whose history answer waits for the test to release it.
struct Gated {
    calls: AtomicUsize,
    entered: Notify,
    release: Notify,
    reply: Mutex<Value>,
}

impl Gated {
    fn new(reply: Value) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Notify::new(),
            reply: Mutex::new(reply),
        })
    }
}

#[async_trait]
impl FlowClient for Gated {
    async fn api(&self, _: ApiCall) -> Value {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        self.reply.lock().clone()
    }

    async fn telegram(&self, _: &str) -> Value {
        Value::Null
    }

    async fn whatsapp(&self, _: &str, _: Option<&str>) -> Value {
        Value::Null
    }
}

fn req(symbol: &str) -> History {
    History {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        interval: "D".into(),
        start_date: Some("2026-10-01".into()),
        end_date: Some("2026-10-29".into()),
        source: "api".into(),
    }
}

fn rows() -> Value {
    json!({"status": "success", "data": [{"timestamp": 1, "close": 1.0}]})
}

#[test]
fn the_web_defaults_are_constants() {
    assert_eq!(HISTORY_CACHE_TTL, Duration::from_secs(30));
    assert_eq!(HISTORY_CACHE_MAXSIZE, 256);
    assert_eq!(HISTORY_FLIGHT_TIMEOUT, Duration::from_secs(60));
    assert_eq!(crate::flow::indicator_service::MAX_HISTORY_BARS, 200);
    assert_eq!(
        crate::flow::indicator_service::MAX_HISTORY_CALENDAR_DAYS,
        4000
    );
}

/// Two callers missing at once make one call and both get its answer, an
/// error included; an error is not kept.
#[tokio::test]
async fn concurrent_misses_share_one_call_and_its_failure() {
    let cache = Arc::new(HistoryCache::new(
        HISTORY_CACHE_TTL,
        8,
        HISTORY_FLIGHT_TIMEOUT,
    ));
    let client = Gated::new(json!({"status": "error", "error": "Too many requests", "code": 429}));
    let (c1, k1) = (cache.clone(), client.clone());
    let first = tokio::spawn(async move { c1.fetch(k1.as_ref(), "anon", &req("SBIN")).await });
    client.entered.notified().await;
    let (c2, k2) = (cache.clone(), client.clone());
    let second = tokio::spawn(async move { c2.fetch(k2.as_ref(), "anon", &req("SBIN")).await });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    client.release.notify_one();
    let (a, b) = (first.await.ok(), second.await.ok());
    assert_eq!(a, b);
    assert_eq!(a.map(|v| v["code"].clone()), Some(json!(429)));
    assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    assert_eq!(cache.len(), 0, "an error stays retryable");
}

/// A kept answer is reused for 30 seconds, then fetched again; an empty
/// range is not kept; the bound evicts the oldest; another account never
/// reads it.
#[tokio::test(start_paused = true)]
async fn answers_expire_are_bounded_and_belong_to_one_account() {
    let cache = HistoryCache::new(HISTORY_CACHE_TTL, 2, HISTORY_FLIGHT_TIMEOUT);
    let client = FakeFlowClient::new();
    client.reply("history", rows());
    cache.fetch(&client, "anon", &req("SBIN")).await;
    cache.fetch(&client, "anon", &req("SBIN")).await;
    assert_eq!(client.calls().len(), 1);
    cache.fetch(&client, "other", &req("SBIN")).await;
    assert_eq!(client.calls().len(), 2, "keyed by account");
    tokio::time::advance(Duration::from_secs(31)).await;
    cache.fetch(&client, "anon", &req("SBIN")).await;
    assert_eq!(client.calls().len(), 3, "expired after 30 s");
    cache.fetch(&client, "anon", &req("INFY")).await;
    cache.fetch(&client, "anon", &req("TCS")).await;
    assert!(cache.len() <= 2, "bounded");
    let empty = FakeFlowClient::new();
    empty.reply("history", json!({"status": "success", "data": []}));
    cache.clear();
    cache.fetch(&empty, "anon", &req("SBIN")).await;
    cache.fetch(&empty, "anon", &req("SBIN")).await;
    assert_eq!(empty.calls().len(), 2, "an empty range stays retryable");
    let off = HistoryCache::new(Duration::ZERO, 2, HISTORY_FLIGHT_TIMEOUT);
    off.fetch(&client, "anon", &req("SBIN")).await;
    off.fetch(&client, "anon", &req("SBIN")).await;
    assert_eq!(client.calls().len(), 7, "a zero TTL disables the cache");
}

/// A caller whose leader never answers fetches itself after the wait.
#[tokio::test(start_paused = true)]
async fn a_waiter_fetches_itself_when_the_leader_is_stuck() {
    let cache = Arc::new(HistoryCache::new(
        HISTORY_CACHE_TTL,
        8,
        Duration::from_secs(60),
    ));
    let stuck = Gated::new(rows());
    let (c1, k1) = (cache.clone(), stuck.clone());
    let leader = tokio::spawn(async move { c1.fetch(k1.as_ref(), "anon", &req("SBIN")).await });
    stuck.entered.notified().await;
    let quick = FakeFlowClient::new();
    quick.reply("history", rows());
    let got = cache.fetch(&quick, "anon", &req("SBIN")).await;
    assert_eq!(got, rows());
    assert_eq!(quick.calls().len(), 1);
    leader.abort();
}

/// The Indicator, Bar Offset and Prior Period OHLC nodes share one fetch
/// for one request within a run; the History node never reads the cache.
#[tokio::test]
async fn nodes_share_the_cached_fetch_but_history_does_not() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    let bars: Vec<Value> = (0..60)
        .map(|i| json!({"timestamp": 1_788_000_000 + i * 86_400, "open": 1.0, "high": 2.0, "low": 0.5, "close": 1.0 + i as f64, "volume": 10}))
        .collect();
    client.reply("history", json!({"status": "success", "data": bars}));
    let c = ctx(client.clone());
    let ind = json!({"symbol": "SBIN", "indicatorName": "sma", "params": "{\"period\": 5}", "lookbackBars": 5});
    run(&c, "indicator", ind.clone()).await;
    run(&c, "indicator", ind).await;
    assert_eq!(client.calls_to("history").len(), 1);
    let h = json!({"symbol": "SBIN", "interval": "D", "startDate": "2026-10-01", "endDate": "2026-10-28"});
    run(&c, "history", h.clone()).await;
    run(&c, "history", h).await;
    assert_eq!(client.calls_to("history").len(), 3);
    assert!(
        logs(&c)
            .iter()
            .any(|(_, m)| m == "sma latest: {'value': 58.0}"),
        "{:?}",
        logs(&c)
    );
}

/// Decision 4: the editor's `1d` reaches `/api/v1/history` as `D`, from
/// every node, while the log keeps what the node says.
#[tokio::test]
async fn one_d_is_sent_as_d() {
    assert_eq!(api_interval("1d"), "D");
    assert_eq!(api_interval("D"), "D");
    assert_eq!(api_interval("5m"), "5m");
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", json!({"status": "success", "data": []}));
    let c = ctx(client.clone());
    run(
        &c,
        "history",
        json!({"symbol": "SBIN", "interval": "1d", "days": 5}),
    )
    .await;
    run(&c, "barOffset", json!({"symbol": "SBIN", "interval": "1d"})).await;
    let sent: Vec<Value> = client
        .calls()
        .iter()
        .map(|c| c.payload["interval"].clone())
        .collect();
    assert_eq!(sent, vec![json!("D"), json!("D")]);
    assert!(logs(&c)
        .iter()
        .any(|(_, m)| m == "Getting history for: SBIN (1d)"));
}
