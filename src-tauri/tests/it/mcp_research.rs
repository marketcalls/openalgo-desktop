//! Research tools over a scripted history: the payload shapes of the web's
//! tools and values equal to the Rust indicator kernels.

use crate::mcp_support::M;
use openalgo_desktop_lib::brokers::types::Candle;
use openalgo_desktop_lib::mcp::store::TokenScope;
use openalgo_desktop_lib::mcp::ta;
use serde_json::{json, Value};

const DAY0: i64 = 1_767_205_800; // 2026-01-01 00:00 IST

fn candles(n: usize) -> Vec<Candle> {
    (0..n)
        .map(|i| {
            let c = 100.0 + (i as f64 * 0.7).sin() * 5.0 + i as f64 * 0.1;
            Candle {
                timestamp: DAY0 + i as i64 * 86_400,
                open: c - 0.5,
                high: c + 1.0,
                low: c - 1.0,
                close: c,
                volume: 1000 + i as i64,
                oi: 0,
            }
        })
        .collect()
}

fn closes(n: usize) -> Vec<f64> {
    candles(n).iter().map(|c| c.close).collect()
}

async fn harness(n: usize) -> (M, String) {
    let m = M::new().await;
    *m.h.mock.history.lock() = Some(Ok(candles(n)));
    let t = m.token(TokenScope::Read);
    (m, t)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn historical_data_is_trimmed_to_the_requested_bars() {
    let (m, t) = harness(60).await;
    let out = m
        .output(
            &t,
            "get_historical_data",
            json!({"symbol": "sbin", "exchange": "NSE", "interval": "D", "bars": 5}),
        )
        .await;
    let d = &out["data"];
    assert_eq!(
        (
            d["count"].clone(),
            d["returned"].clone(),
            d["truncated"].clone()
        ),
        (json!(60), json!(5), json!(true))
    );
    let rows = d["data"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    // Daily index: naive timestamp, as pandas writes it.
    assert_eq!(rows[4]["timestamp"], "2026-02-28T18:30:00.000");
    assert_eq!(rows[4]["volume"], 1059);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn calculate_indicator_matches_the_kernel() {
    let (m, t) = harness(60).await;
    let out = m
        .output(
            &t,
            "calculate_indicator",
            json!({"symbol": "SBIN", "exchange": "NSE", "indicator": "RSI", "params": {"period": 14}, "bars": 3}),
        )
        .await;
    let d = &out["data"];
    let want = ta::last(&ta::rsi_k(&closes(60), 14)).unwrap();
    assert_eq!(d["latest"]["value"], json!(want));
    assert_eq!(d["indicator"], "rsi");
    assert_eq!(d["inputs"], json!(["close"]));
    assert_eq!(d["returned_bars"], 3);
    assert_eq!(d["data"].as_array().unwrap().len(), 3);
    assert_eq!(d["summary"]["value"]["last"], json!(want));
    assert_eq!(d["latest_timestamp"], "2026-02-28T18:30:00");

    let macd = m
        .output(
            &t,
            "calculate_indicator",
            json!({"symbol": "SBIN", "exchange": "NSE", "indicator": "macd"}),
        )
        .await;
    assert!(macd["data"]["latest"]["out2"].is_number(), "{}", macd);

    let kama = m
        .output(
            &t,
            "calculate_indicator",
            json!({"symbol": "SBIN", "exchange": "NSE", "indicator": "kama"}),
        )
        .await;
    assert_eq!(kama["data"]["error"]["error_type"], "not_available");
    let bogus = m
        .output(
            &t,
            "calculate_indicator",
            json!({"symbol": "SBIN", "exchange": "NSE", "indicator": "bogus"}),
        )
        .await;
    assert_eq!(
        bogus["data"]["error"]["message"],
        "unknown indicator 'bogus'"
    );
    let missing = m
        .output(
            &t,
            "calculate_indicator",
            json!({"symbol": "SBIN", "exchange": "NSE", "indicator": "ema"}),
        )
        .await;
    assert_eq!(missing["data"]["error"]["error_type"], "TypeError");
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshots_bundle_latest_values_and_per_item_errors() {
    let (m, t) = harness(60).await;
    let args = json!({"symbol": "SBIN", "exchange": "NSE"});
    let trend = m.output(&t, "get_trend_snapshot", args.clone()).await["data"].clone();
    assert_eq!(trend["bars_loaded"], 60);
    assert_eq!(
        trend["indicators"]["sma_20"],
        json!(ta::last(&ta::sma_k(&closes(60), 20)).unwrap())
    );
    assert_eq!(
        trend["indicators"]["sma_200"],
        json!({"error": "Period (200) cannot be greater than data length (60)"})
    );
    assert_eq!(
        trend["indicators"]["supertrend"].as_array().unwrap().len(),
        2
    );
    assert_eq!(trend["indicators"]["ichimoku"].as_array().unwrap().len(), 5);
    let mom = m.output(&t, "get_momentum_snapshot", args.clone()).await["data"].clone();
    assert_eq!(mom["indicators"]["macd"].as_array().unwrap().len(), 3);
    let vol = m.output(&t, "get_volatility_snapshot", args.clone()).await["data"].clone();
    assert!(
        vol["indicators"]["historical_volatility"].is_number(),
        "{}",
        vol
    );
    let lv = m.output(&t, "get_support_resistance", args).await["data"].clone();
    assert_eq!(lv["levels"]["pivot_points"].as_array().unwrap().len(), 7);
    assert_eq!(lv["period"], 20);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signals_screens_timeframes_and_correlation() {
    let (m, t) = harness(60).await;
    let sig = m
        .output(
            &t,
            "detect_signals",
            json!({"symbol": "SBIN", "exchange": "NSE", "fast": 3, "slow": 8}),
        )
        .await["data"]
        .clone();
    let c = closes(60);
    let f = ta::ema_k(&c, 3);
    let s = ta::ema_k(&c, 8);
    let n = ta::crossover(&f, &s).iter().filter(|x| **x).count()
        + ta::crossunder(&f, &s).iter().filter(|x| **x).count();
    assert_eq!(sig["event_count"], n);
    assert!(n > 0);
    let unknown = m
        .output(
            &t,
            "detect_signals",
            json!({"symbol": "SBIN", "exchange": "NSE", "signal_type": "x"}),
        )
        .await;
    assert_eq!(
        unknown["data"]["error"]["message"],
        "unknown signal_type 'x'"
    );

    let scr = m
        .output(&t, "screen_instruments", json!({"symbols": [{"symbol": "SBIN", "exchange": "NSE"}], "condition": "rsi_above", "value": 0}))
        .await["data"]
        .clone();
    assert_eq!(
        (scr["scanned"].clone(), scr["matched"].clone()),
        (json!(1), json!(1))
    );

    let mtf = m
        .output(
            &t,
            "multi_timeframe_analysis",
            json!({"symbol": "SBIN", "exchange": "NSE", "intervals": ["D"]}),
        )
        .await["data"]
        .clone();
    assert_eq!(mtf["timeframes"]["D"]["bars"], 60);

    let cb = m
        .output(
            &t,
            "correlation_beta",
            json!({"symbol1": "SBIN", "exchange1": "NSE", "symbol2": "SBIN", "exchange2": "NSE"}),
        )
        .await["data"]
        .clone();
    assert_eq!(cb["metrics"]["pearson_full_sample"], json!(1.0));
    assert_eq!(cb["overlapping_bars"], 60);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_history_failure_is_reported_like_the_web() {
    let m = M::new().await;
    *m.h.mock.history.lock() = Some(Err("Broker is down".into()));
    let t = m.token(TokenScope::Read);
    let out = m
        .output(
            &t,
            "get_trend_snapshot",
            json!({"symbol": "SBIN", "exchange": "NSE"}),
        )
        .await;
    let msg = out["data"]["error"]["message"].as_str().unwrap();
    assert!(
        msg.starts_with("Error getting trend snapshot: history error: {"),
        "{}",
        msg
    );
    assert_eq!(out["data"]["error"]["error_type"], "ValueError");
    let _: Value = out;
    m.h.shutdown().await;
}
