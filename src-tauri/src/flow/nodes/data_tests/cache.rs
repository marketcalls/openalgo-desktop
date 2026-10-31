//! The history cache (N-dataCommon-11, N-dataCommon-12; web
//! `indicator_service.fetch_history_cached`): one call for identical
//! requests, shared by concurrent callers, kept 30 seconds, only when it has
//! rows, never more than its bound; and the nodes that use it.

use super::support::{ctx, logs, run, ServerLog};
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::client::{ApiCall, FlowClient, History};
use crate::flow::nodes::history::{
    api_interval, broker_account_tag, shared_cache, HistoryCache, HISTORY_CACHE_MAXSIZE,
    HISTORY_CACHE_TTL, HISTORY_FLIGHT_TIMEOUT,
};
use crate::services::tunnel::fake::capture_logs;
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
    let first =
        tokio::spawn(async move { c1.fetch(k1.as_ref(), "anon", &req("SBIN"), None).await });
    client.entered.notified().await;
    let (c2, k2) = (cache.clone(), client.clone());
    let second =
        tokio::spawn(async move { c2.fetch(k2.as_ref(), "anon", &req("SBIN"), None).await });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    client.release.notify_one();
    let (a, b) = (first.await.ok(), second.await.ok());
    assert_eq!(a, b);
    assert_eq!(a.map(|v| v["code"].clone()), Some(json!(429)));
    assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    assert_eq!(cache.held(), 0, "an error stays retryable");
}

/// A kept answer is reused for 30 seconds, then fetched again; an empty
/// range is not kept; the bound evicts the oldest; another account never
/// reads it.
#[tokio::test(start_paused = true)]
async fn answers_expire_are_bounded_and_belong_to_one_account() {
    let cache = HistoryCache::new(HISTORY_CACHE_TTL, 2, HISTORY_FLIGHT_TIMEOUT);
    let client = FakeFlowClient::new();
    client.reply("history", rows());
    cache.fetch(&client, "anon", &req("SBIN"), None).await;
    cache.fetch(&client, "anon", &req("SBIN"), None).await;
    assert_eq!(client.calls().len(), 1);
    cache.fetch(&client, "other", &req("SBIN"), None).await;
    assert_eq!(client.calls().len(), 2, "keyed by account");
    tokio::time::advance(Duration::from_secs(31)).await;
    cache.fetch(&client, "anon", &req("SBIN"), None).await;
    assert_eq!(client.calls().len(), 3, "expired after 30 s");
    cache.fetch(&client, "anon", &req("INFY"), None).await;
    cache.fetch(&client, "anon", &req("TCS"), None).await;
    assert!(cache.held() <= 2, "bounded");
    let empty = FakeFlowClient::new();
    empty.reply("history", json!({"status": "success", "data": []}));
    cache.clear();
    cache.fetch(&empty, "anon", &req("SBIN"), None).await;
    cache.fetch(&empty, "anon", &req("SBIN"), None).await;
    assert_eq!(empty.calls().len(), 2, "an empty range stays retryable");
    let off = HistoryCache::new(Duration::ZERO, 2, HISTORY_FLIGHT_TIMEOUT);
    off.fetch(&client, "anon", &req("SBIN"), None).await;
    off.fetch(&client, "anon", &req("SBIN"), None).await;
    assert_eq!(client.calls().len(), 7, "a zero TTL disables the cache");
}

/// The cache key's account part (IND-011, decision 21): the first 12 hex
/// digits of `sha256("broker:user")` of the broker session, one per
/// account, never the ids themselves. Pinned to values computed apart.
#[test]
fn the_account_tag_is_a_short_hash_of_the_broker_account() {
    assert_eq!(broker_account_tag("zerodha", "AB1234"), "dc4a384014d7");
    assert_eq!(broker_account_tag("zerodha", "CD5678"), "bd7416bf56b5");
    assert_eq!(broker_account_tag("dhan", "AB1234"), "d9ab8eb7fd4e");
    let tag = broker_account_tag("zerodha", "AB1234");
    assert_eq!(tag.len(), 12);
    assert!(tag.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!tag.contains("AB1234") && !tag.contains("zerodha"));
}

/// `clear_history_cache` (IND-014): every kept answer is dropped, so the
/// next identical request reaches the client again.
#[tokio::test]
async fn clear_drops_every_cached_answer() {
    let cache = HistoryCache::new(HISTORY_CACHE_TTL, 8, HISTORY_FLIGHT_TIMEOUT);
    let client = FakeFlowClient::new();
    client.reply("history", rows());
    cache.fetch(&client, "anon", &req("SBIN"), None).await;
    cache.fetch(&client, "anon", &req("INFY"), None).await;
    cache.fetch(&client, "anon", &req("SBIN"), None).await;
    assert_eq!(cache.held(), 2);
    assert_eq!(
        client.calls().len(),
        2,
        "the second SBIN is served from the cache"
    );
    cache.clear();
    assert_eq!(cache.held(), 0);
    cache.fetch(&client, "anon", &req("SBIN"), None).await;
    assert_eq!(
        client.calls().len(),
        3,
        "after clear the request is made again"
    );
}

/// A caller whose leader never answers fetches itself after the wait, and
/// says so in the server log at warning level, as the web does
/// (N-dataCommon-12).
#[tokio::test(start_paused = true)]
async fn a_waiter_fetches_itself_when_the_leader_is_stuck() {
    let log = ServerLog::capture();
    let cache = Arc::new(HistoryCache::new(
        HISTORY_CACHE_TTL,
        8,
        Duration::from_secs(60),
    ));
    let logs = capture_logs();
    let stuck = Gated::new(rows());
    let (c1, k1) = (cache.clone(), stuck.clone());
    let leader =
        tokio::spawn(async move { c1.fetch(k1.as_ref(), "anon", &req("SBIN"), None).await });
    stuck.entered.notified().await;
    let quick = FakeFlowClient::new();
    quick.reply("history", rows());
    let got = cache.fetch(&quick, "anon", &req("SBIN"), None).await;
    assert_eq!(got, rows());
    assert_eq!(quick.calls().len(), 1);
    assert!(
        log.has(
            "WARN",
            "History single-flight wait timed out for SBIN D; fetching directly"
        ),
        "{:?}",
        log.lines()
    );
    leader.abort();
    // The web's warning (IND-013).
    let text = logs.text();
    assert!(
        text.lines().any(|l| l.contains("WARN")
            && l.ends_with("History single-flight wait timed out for SBIN D; fetching directly")),
        "{text}"
    );
}

/// After a leader's call fails and its waiters share the failure, the
/// flight is retired: the next identical request makes its own call
/// rather than waiting on a call nobody makes (IND-013).
#[tokio::test]
async fn a_shared_failure_retires_the_flight() {
    let cache = Arc::new(HistoryCache::new(
        HISTORY_CACHE_TTL,
        8,
        HISTORY_FLIGHT_TIMEOUT,
    ));
    let failing = Gated::new(json!({"status": "error", "error": "Too many requests"}));
    let (c1, k1) = (cache.clone(), failing.clone());
    let leader =
        tokio::spawn(async move { c1.fetch(k1.as_ref(), "anon", &req("SBIN"), None).await });
    failing.entered.notified().await;
    let (c2, k2) = (cache.clone(), failing.clone());
    let waiter =
        tokio::spawn(async move { c2.fetch(k2.as_ref(), "anon", &req("SBIN"), None).await });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    failing.release.notify_one();
    let (a, b) = (leader.await.ok(), waiter.await.ok());
    assert_eq!(a, b, "the waiter shares the failure");
    assert_eq!(failing.calls.load(Ordering::SeqCst), 1);
    let logs = capture_logs();
    let next = FakeFlowClient::new();
    next.reply("history", rows());
    assert_eq!(cache.fetch(&next, "anon", &req("SBIN"), None).await, rows());
    assert_eq!(next.calls().len(), 1);
    // A flight left behind would send this caller to wait on it first.
    assert!(
        !logs.text().contains("History single-flight wait"),
        "the next request waited on a retired flight: {}",
        logs.text()
    );
}

/// Every field of a request is part of its key (IND-011): another exchange,
/// interval, start, end or source is a new call; the same request again is
/// a hit, with the web's debug line `History cache hit: ...` (IND-013).
#[tokio::test]
async fn every_request_field_is_part_of_the_cache_key() {
    let logs = capture_logs();
    let cache = HistoryCache::new(HISTORY_CACHE_TTL, 16, HISTORY_FLIGHT_TIMEOUT);
    let client = FakeFlowClient::new();
    client.reply("history", rows());
    let variants = [
        req("SBIN"),
        History {
            exchange: "BSE".into(),
            ..req("SBIN")
        },
        History {
            interval: "5m".into(),
            ..req("SBIN")
        },
        History {
            start_date: Some("2026-09-01".into()),
            ..req("SBIN")
        },
        History {
            end_date: Some("2026-10-28".into()),
            ..req("SBIN")
        },
        History {
            source: "db".into(),
            ..req("SBIN")
        },
    ];
    for (i, variant) in variants.iter().enumerate() {
        cache.fetch(&client, "anon", variant, None).await;
        assert_eq!(
            client.calls().len(),
            i + 1,
            "{variant:?} is its own request"
        );
    }
    for variant in &variants {
        cache.fetch(&client, "anon", variant, None).await;
    }
    assert_eq!(
        client.calls().len(),
        variants.len(),
        "each answered from the cache"
    );
    let text = logs.text();
    for line in [
        "History cache hit: SBIN D (api)",
        "History cache hit: SBIN 5m (api)",
        "History cache hit: SBIN D (db)",
    ] {
        assert!(
            text.lines()
                .any(|l| l.contains("DEBUG") && l.ends_with(line)),
            "{line} missing from {text}"
        );
    }
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

/// Runs without the app (tests, golden replays) never read each other's
/// answers: each is its own client's, as the web's recorder clears the
/// cache per scenario. A run with the app shares by broker account.
#[tokio::test(start_paused = true)]
async fn runs_without_the_app_keep_their_own_answers() {
    shared_cache().clear();
    let ind = json!({"symbol": "SBIN", "indicatorName": "sma", "params": "{\"period\": 2}", "lookbackBars": 5});
    for close in [10.0, 20.0] {
        let client = Arc::new(FakeFlowClient::new());
        let bars: Vec<Value> = (0..10)
            .map(|i| json!({"timestamp": 1_788_000_000 + i * 86_400, "close": close}))
            .collect();
        client.reply("history", json!({"status": "success", "data": bars}));
        let c = ctx(client.clone());
        let out = run(&c, "indicator", ind.clone()).await;
        assert_eq!(
            client.calls_to("history").len(),
            1,
            "a new run fetches its own"
        );
        assert_eq!(
            out.value().map(|v| v["latest"]["value"].clone()),
            Some(json!(close))
        );
    }
}

/// Sixty daily bars ending well before the clock's day.
fn daily_bars() -> Value {
    let bars: Vec<Value> = (0..60)
        .map(|i| json!({"timestamp": 1_788_000_000 + i * 86_400, "open": 1.0, "high": 2.0, "low": 0.5, "close": 1.0 + i as f64, "volume": 10}))
        .collect();
    json!({"status": "success", "data": bars})
}

/// N-priorPeriodOhlc-02: Prior Period OHLC reads history through the
/// shared cache, so a second identical node in the run makes no call.
#[tokio::test]
async fn prior_period_ohlc_reads_through_the_cache() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", daily_bars());
    let c = ctx(client.clone());
    let node = json!({"symbol": "SBIN", "period": "previous_day"});
    let first = run(&c, "priorPeriodOhlc", node.clone()).await;
    let second = run(&c, "priorPeriodOhlc", node).await;
    assert_eq!(first.value(), second.value());
    assert_eq!(first.value().map(|v| v["pdc"].clone()), Some(json!(60.0)));
    assert_eq!(client.calls_to("history").len(), 1);
}

/// N-barOffset-02: Bar Offset reads history through the shared cache, so a
/// second identical node in the run makes no call.
#[tokio::test]
async fn bar_offset_reads_through_the_cache() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", daily_bars());
    let c = ctx(client.clone());
    let node = json!({"symbol": "SBIN", "interval": "D", "offsetBars": 2});
    let first = run(&c, "barOffset", node.clone()).await;
    let second = run(&c, "barOffset", node).await;
    assert_eq!(first.value().map(|v| v["close"].clone()), Some(json!(58.0)));
    assert_eq!(first.value(), second.value());
    assert_eq!(client.calls_to("history").len(), 1);
}
