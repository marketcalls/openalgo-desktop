//! One test per resource-cap bypass of the research tools: each must be
//! refused before anything is fetched, and the shared budget (per-token
//! in-flight cap, process-wide slots, the whole fan-out reserved up front)
//! must hold.

use crate::mcp_support::M;
use openalgo_desktop_lib::brokers::mock::MockCall;
use openalgo_desktop_lib::brokers::types::Candle;
use openalgo_desktop_lib::mcp::dispatch::UPSTREAM_LIMIT;
use openalgo_desktop_lib::mcp::research::{MAX_PER_TOKEN, MAX_SCREEN_SYMBOLS};
use openalgo_desktop_lib::mcp::store::{self, TokenScope};
use serde_json::{json, Value};

fn history_calls(m: &M) -> usize {
    m.h.mock
        .calls
        .lock()
        .iter()
        .filter(|c| matches!(c, MockCall::History(_)))
        .count()
}

fn candles() -> Vec<Candle> {
    (0..40)
        .map(|i| Candle {
            timestamp: 1_767_205_800 + i * 86_400,
            open: 100.0,
            high: 101.0 + i as f64 % 3.0,
            low: 99.0,
            close: 100.0 + (i as f64 * 0.5).sin(),
            volume: 10,
            oi: 0,
        })
        .collect()
}

fn watchlist(n: usize) -> Value {
    json!((0..n)
        .map(|_| json!({"symbol": "SBIN", "exchange": "NSE"}))
        .collect::<Vec<_>>())
}

async fn harness() -> (M, String) {
    let m = M::new().await;
    *m.h.mock.history.lock() = Some(Ok(candles()));
    let t = m.token(TokenScope::Read);
    (m, t)
}

fn upstream_key(m: &M, token: &str) -> String {
    let id = store::find_token(&m.h.ctx.sqlite.conn().unwrap(), token)
        .unwrap()
        .unwrap()
        .id;
    format!("mcp-token-{}|upstream", id)
}

async fn refused_unfetched(m: &M, t: &str, tool: &str, args: Value, kind: &str) {
    let before = history_calls(m);
    let out = m.output(t, tool, args.clone()).await;
    assert_eq!(
        out["data"]["error"]["error_type"], kind,
        "{} {}: {}",
        tool, args, out
    );
    assert_eq!(history_calls(m), before, "{} fetched before refusing", tool);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_ended_or_malformed_range_is_checked_as_resolved() {
    let (m, t) = harness().await;
    // start_date alone: the range runs to today.
    refused_unfetched(
        &m,
        &t,
        "get_trend_snapshot",
        json!({"symbol": "SBIN", "exchange": "NSE", "start_date": "1990-01-01"}),
        "limit_exceeded",
    )
    .await;
    // An end date far in the future.
    refused_unfetched(
        &m,
        &t,
        "detect_signals",
        json!({"symbol": "SBIN", "exchange": "NSE",
               "start_date": "2026-01-01", "end_date": "2099-01-01"}),
        "limit_exceeded",
    )
    .await;
    // A date the window code could not read is refused, not passed on.
    refused_unfetched(
        &m,
        &t,
        "get_momentum_snapshot",
        json!({"symbol": "SBIN", "exchange": "NSE", "start_date": "01-01-1990"}),
        "limit_exceeded",
    )
    .await;
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn range_times_interval_is_capped_before_expansion() {
    let (m, t) = harness().await;
    // 3000 days of 1-minute bars: inside the day cap, about a million bars.
    refused_unfetched(
        &m,
        &t,
        "get_historical_data",
        json!({"symbol": "SBIN", "exchange": "NSE", "interval": "1m", "lookback_days": 3000}),
        "limit_exceeded",
    )
    .await;
    // Omitted intervals default to 5m/15m/1h/D: the 5m leg is too large.
    refused_unfetched(
        &m,
        &t,
        "multi_timeframe_analysis",
        json!({"symbol": "SBIN", "exchange": "NSE", "lookback_days": 2000}),
        "limit_exceeded",
    )
    .await;
    // The same span of daily bars is fine.
    let ok = m
        .output(
            &t,
            "get_historical_data",
            json!({"symbol": "SBIN", "exchange": "NSE", "interval": "D", "lookback_days": 2000}),
        )
        .await;
    assert!(ok["data"]["count"].is_number(), "{}", ok);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_strings_lists_and_parameters_are_refused() {
    let (m, t) = harness().await;
    let with = |extra: Value| {
        let mut v = json!({"symbol": "SBIN", "exchange": "NSE", "indicator": "rsi"});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    refused_unfetched(
        &m,
        &t,
        "calculate_indicator",
        with(json!({"inputs": vec!["close"; 1000]})),
        "limit_exceeded",
    )
    .await;
    refused_unfetched(
        &m,
        &t,
        "calculate_indicator",
        with(json!({"symbol": "S".repeat(10_000)})),
        "limit_exceeded",
    )
    .await;
    let many: serde_json::Map<String, Value> =
        (0..20).map(|k| (format!("p{}", k), json!(1))).collect();
    refused_unfetched(
        &m,
        &t,
        "calculate_indicator",
        with(json!({"params": many})),
        "limit_exceeded",
    )
    .await;
    refused_unfetched(
        &m,
        &t,
        "screen_instruments",
        json!({"symbols": [{"symbol": "X".repeat(500), "exchange": "NSE"}]}),
        "limit_exceeded",
    )
    .await;
    // An absurd indicator parameter is a refusal, not an overflow: an
    // integer far past any window, and a float where an integer is due.
    let out = m
        .output(
            &t,
            "calculate_indicator",
            json!({"symbol": "SBIN", "exchange": "NSE", "indicator": "ichimoku",
                   "params": {"displacement": -9_000_000_000_000_000_000i64}}),
        )
        .await;
    assert_eq!(out["data"]["error"]["error_type"], "ValueError", "{}", out);
    let out = m
        .output(
            &t,
            "calculate_indicator",
            json!({"symbol": "SBIN", "exchange": "NSE", "indicator": "ichimoku",
                   "params": {"displacement": -9.0e18}}),
        )
        .await;
    assert_eq!(out["data"]["error"]["error_type"], "TypeError", "{}", out);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_whole_fan_out_is_reserved_before_work_starts() {
    let (m, t) = harness().await;
    let key = upstream_key(&m, &t);
    let (limit, window) = UPSTREAM_LIMIT;
    // Leave ten calls in this minute's budget.
    assert!(m.h.ctx.mcp.reserve(&key, limit - 10, limit, window));
    refused_unfetched(
        &m,
        &t,
        "screen_instruments",
        json!({"symbols": watchlist(MAX_SCREEN_SYMBOLS)}),
        "rate_limited",
    )
    .await;
    // The refused call took nothing; a call that fits runs and uses it all.
    assert_eq!(m.h.ctx.mcp.remaining(&key, limit, window), 10);
    let ok = m
        .output(&t, "screen_instruments", json!({"symbols": watchlist(10)}))
        .await;
    assert_eq!(ok["data"]["scanned"], 10);
    assert_eq!(m.h.ctx.mcp.remaining(&key, limit, window), 0);
    assert_eq!(history_calls(&m), 10);
    // Now all nine exchanges of the instrument master do not fit.
    let out = m.output(&t, "get_instruments", json!({})).await;
    assert_eq!(
        out["data"]["error"]["error_type"], "rate_limited",
        "{}",
        out
    );
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_token_cannot_hold_every_research_slot() {
    let (m, t) = harness().await;
    let key = upstream_key(&m, &t);
    let held: Vec<_> = (0..MAX_PER_TOKEN)
        .map(|_| m.h.ctx.mcp.enter(&key, MAX_PER_TOKEN).unwrap())
        .collect();
    refused_unfetched(
        &m,
        &t,
        "get_trend_snapshot",
        json!({"symbol": "SBIN", "exchange": "NSE"}),
        "busy",
    )
    .await;
    let out = m
        .output(&t, "get_instruments", json!({"exchange": "NSE"}))
        .await;
    assert_eq!(out["data"]["error"]["error_type"], "busy");
    // Another token is not affected.
    let other = m.token(TokenScope::Read);
    let ok = m
        .output(
            &other,
            "get_trend_snapshot",
            json!({"symbol": "SBIN", "exchange": "NSE"}),
        )
        .await;
    assert!(ok["data"]["indicators"].is_object(), "{}", ok);
    drop(held);
    assert_eq!(m.h.ctx.mcp.in_flight(&key), 0);
    m.h.shutdown().await;
}
