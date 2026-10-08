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

/// The call is refused before anything is fetched: by the tool's limits
/// (`kind` in its output) or, for a value that is not even of the declared
/// type, by argument binding (the web's `bad_arguments`).
async fn refused_unfetched(m: &M, t: &str, tool: &str, args: Value, kind: &str) {
    let before = history_calls(m);
    let reply = m.call(t, tool, args.clone()).await;
    match reply["result"]["content"][0]["text"].as_str() {
        Some(text) => {
            let out: Value = serde_json::from_str(text).unwrap();
            assert_eq!(
                out["data"]["error"]["error_type"], kind,
                "{} {}: {}",
                tool, args, out
            );
        }
        None => assert_eq!(
            reply["error"]["data"]["reason"], "Invalid arguments. Check the tool schema.",
            "{} {}: {}",
            tool, args, reply
        ),
    }
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

// ---------------------------------------------------------------------
// Extreme values and parser variants: parsed once, checked on that value.
// ---------------------------------------------------------------------

fn snap(extra: Value) -> Value {
    let mut v = json!({"symbol": "SBIN", "exchange": "NSE"});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extreme_numbers_are_refused_without_overflow() {
    let (m, t) = harness().await;
    for extra in [
        json!({"lookback_bars": u64::MAX}),
        json!({"lookback_bars": i64::MIN}),
        json!({"lookback_bars": i64::MAX}),
        json!({"lookback_days": i64::MIN}),
        json!({"lookback_days": -3661}),
        json!({"lookback_bars": 1e300}),
        json!({"lookback_bars": -1e300}),
        json!({"lookback_bars": "-9223372036854775808"}),
        json!({"period": i64::MIN}),
        json!({"limit": u64::MAX}),
    ] {
        refused_unfetched(&m, &t, "detect_signals", snap(extra), "limit_exceeded").await;
    }
    let out = m
        .output(&t, "get_instruments", json!({"limit": i64::MIN}))
        .await;
    assert_eq!(
        out["data"]["error"]["error_type"], "limit_exceeded",
        "{}",
        out
    );
    // A negative lookback that points into the future still counts its span.
    refused_unfetched(
        &m,
        &t,
        "get_historical_data",
        snap(json!({"interval": "1m", "lookback_days": -3000})),
        "limit_exceeded",
    )
    .await;
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn date_and_number_spellings_that_could_parse_differently_are_refused() {
    let (m, t) = harness().await;
    for d in [
        "2026-1-1",
        "2026/01/01",
        "20260101",
        " 2026-01-01",
        "2026-01-01 ",
        "+12026-01-01",
        "0001-01-01",
        "9999-12-31",
        "01-01-2026",
        "2026-01-01T00:00:00",
    ] {
        refused_unfetched(
            &m,
            &t,
            "get_trend_snapshot",
            snap(json!({"start_date": d})),
            "limit_exceeded",
        )
        .await;
        refused_unfetched(
            &m,
            &t,
            "get_trend_snapshot",
            snap(json!({"end_date": d})),
            "limit_exceeded",
        )
        .await;
    }
    for bars in [
        json!("5001"),
        json!("1e9"),
        json!(" 5"),
        json!("5.0"),
        json!(5000.5),
        json!("+5"),
    ] {
        refused_unfetched(
            &m,
            &t,
            "calculate_indicator",
            snap(json!({"indicator": "rsi", "lookback_bars": bars})),
            "limit_exceeded",
        )
        .await;
    }
    // Canonical spellings work, and are what is fetched.
    let ok = m
        .output(
            &t,
            "calculate_indicator",
            snap(json!({"indicator": "rsi", "lookback_bars": "30", "bars": 5.0})),
        )
        .await;
    assert_eq!(ok["data"]["returned_bars"], 5, "{}", ok);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interval_spellings_that_could_expand_later_are_refused() {
    let (m, t) = harness().await;
    for itv in [
        "1 m", " 1m", "1m ", "1min", "1e3m", "1000m", "0m", "m", "d", "1d", "-1m", "1.5m",
    ] {
        refused_unfetched(
            &m,
            &t,
            "get_trend_snapshot",
            snap(json!({"interval": itv})),
            "limit_exceeded",
        )
        .await;
    }
    refused_unfetched(
        &m,
        &t,
        "multi_timeframe_analysis",
        snap(json!({"intervals": ["D", "1 m"]})),
        "limit_exceeded",
    )
    .await;
    // Seconds are counted as seconds: the default window is far too many.
    refused_unfetched(
        &m,
        &t,
        "get_trend_snapshot",
        snap(json!({"interval": "1s"})),
        "limit_exceeded",
    )
    .await;
    // "1m" is minutes and "W" weeks: both fine at the default window (monthly
    // bars at the default 252-bar window span about 22 years: refused).
    for itv in ["W", "1m"] {
        let out = m
            .output(&t, "get_trend_snapshot", snap(json!({"interval": itv})))
            .await;
        assert!(out["data"]["indicators"].is_object(), "{}: {}", itv, out);
    }
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_duplicate_key_is_one_value_for_the_check_and_the_work() {
    let (m, t) = harness().await;
    // serde keeps the last of duplicate keys; the request is parsed once,
    // so the check sees the same 100000 the work would.
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_historical_data","arguments":{"symbol":"SBIN","exchange":"NSE","interval":"D","bars":5,"bars":100000}}}"#;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", t))
        .body(axum::body::Body::from(body))
        .unwrap();
    let before = history_calls(&m);
    let (s, _, bytes) = m.raw(req, crate::mcp_support::LOCAL).await;
    assert_eq!(s, axum::http::StatusCode::OK);
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let out: Value =
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        out["data"]["error"]["error_type"], "limit_exceeded",
        "{}",
        out
    );
    assert_eq!(history_calls(&m), before);
    m.h.shutdown().await;
}
