//! The history-reading nodes' details the goldens leave open: how a failed
//! history answer is worded (`_history_error`), which bar is "today" at
//! the IST date split, the Indicator's use of today's bar, its params
//! object read as stored, and the settings that reach its fetch and log.

use super::support::{ctx, ctx_at, logs, run};
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::env::NodeEnv;
use crate::flow::nodes::history::{history_error, shared_cache};
use serde_json::{json, Value};
use std::sync::Arc;

/// IST midnight of `day` October 2026, as epoch seconds.
fn oct(day: i64) -> i64 {
    1_793_212_200 + (day - 29) * 86_400
}

/// Daily bars for `days` of October 2026, the close equal to the day.
fn daily(days: std::ops::RangeInclusive<i64>) -> Value {
    let bars: Vec<Value> = days
        .map(|d| json!({"timestamp": oct(d), "open": d, "high": d + 1, "low": d - 1, "close": d, "volume": 100}))
        .collect();
    json!({"status": "success", "data": bars})
}

const HINT: &str = " If your broker does not support the 'D' interval, use the Intervals node to list supported ones, or set Source to Historify DB to resample it locally.";

/// N-dataCommon-13: a history answer fails on `status` error or on any
/// truthy `error`; its detail is `message`, then `error`, then `unknown
/// error`; the interval hint is left out for the Historify source.
#[test]
fn a_failed_history_answer_is_worded_as_the_web_words_it() {
    let failed = |r: Value, source: &str| history_error(&r, "SBIN", "D", source);
    assert_eq!(
        failed(json!({"status": "error"}), "api"),
        Some(format!(
            "History fetch failed for SBIN (D): unknown error.{}",
            HINT
        ))
    );
    assert_eq!(
        failed(
            json!({"status": "error", "message": "", "error": null}),
            "db"
        ),
        Some("History fetch failed for SBIN (D): unknown error.".to_string())
    );
    assert_eq!(
        failed(
            json!({"status": "error", "message": "Session expired", "error": "Unauthorised"}),
            "db"
        ),
        Some("History fetch failed for SBIN (D): Session expired.".to_string())
    );
    assert_eq!(
        failed(
            json!({"status": "error", "message": "", "error": "Unauthorised"}),
            "db"
        ),
        Some("History fetch failed for SBIN (D): Unauthorised.".to_string())
    );
    // A truthy error fails the answer whatever its status says.
    assert_eq!(
        failed(
            json!({"status": "success", "error": "Interval not supported", "data": []}),
            "db"
        ),
        Some("History fetch failed for SBIN (D): Interval not supported.".to_string())
    );
    assert_eq!(
        failed(json!({"error": "Interval not supported"}), "api"),
        Some(format!(
            "History fetch failed for SBIN (D): Interval not supported.{}",
            HINT
        ))
    );
    assert_eq!(
        failed(json!({"status": "success", "error": "", "data": []}), "api"),
        None
    );
    assert_eq!(failed(json!({"data": []}), "api"), None);
}

/// N-dataCommon-13 through a node: an answer carrying only a truthy
/// `error` fails Bar Offset in the web's words.
#[tokio::test]
async fn a_status_less_error_fails_the_node() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", json!({"error": "Interval not supported"}));
    let c = ctx(client);
    let out = run(&c, "barOffset", json!({"symbol": "SBIN"})).await;
    let message = format!(
        "History fetch failed for SBIN (D): Interval not supported.{}",
        HINT
    );
    assert_eq!(
        out.value().cloned(),
        Some(json!({"status": "error", "message": message.clone()}))
    );
    assert!(logs(&c).iter().any(|(l, m)| l == "error" && *m == message));
}

/// N-dataCommon-10, decision 1: at 00:30 IST on 29 October (still 28
/// October in UTC) the 29 October daily bar is today's, still forming:
/// Prior Period OHLC's previous day and Bar Offset's last closed bar are
/// both 28 October.
#[tokio::test]
async fn today_is_the_ist_date_at_the_date_split() {
    for (node, data) in [
        (
            "priorPeriodOhlc",
            json!({"symbol": "SBIN", "period": "previous_day"}),
        ),
        ("barOffset", json!({"symbol": "SBIN", "interval": "D"})),
    ] {
        shared_cache().clear();
        let client = Arc::new(FakeFlowClient::new());
        client.reply("history", daily(26..=29));
        let c = ctx_at(client, "2026-10-29T00:30:00", "w", NodeEnv::default());
        let out = run(&c, node, data).await;
        let v = out.value().cloned().unwrap_or(Value::Null);
        assert_eq!(v["close"], json!(28), "{} {}", node, v);
        if node == "priorPeriodOhlc" {
            assert_eq!(
                (v["date"].clone(), v["pdh"].clone()),
                (json!("2026-10-28"), json!(29))
            );
        } else {
            assert_eq!(v["timestamp"], json!(oct(28)));
        }
    }
}

/// N-indicator-10: the Indicator computes over every bar history returns,
/// today's forming bar included (only Bar Offset drops it).
#[tokio::test]
async fn the_indicator_uses_todays_bar() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", daily(20..=29));
    let c = ctx(client);
    let out = run(
        &c,
        "indicator",
        json!({"symbol": "SBIN", "indicatorName": "sma", "params": "{\"period\": 2}", "lookbackBars": 10}),
    )
    .await;
    assert_eq!(
        out.value().map(|v| v["latest"].clone()),
        Some(json!({"value": 28.5})),
        "{:?}",
        out
    );
}

/// IND-071: params stored as an object are used as stored: a `{{...}}`
/// inside is never interpolated (the JSON text form is, as a whole).
#[tokio::test]
async fn object_params_are_not_interpolated() {
    let node = |params: Value| json!({"symbol": "SBIN", "indicatorName": "sma", "params": params, "lookbackBars": 10});
    let mut answers = Vec::new();
    for params in [json!("{\"period\": {{p}}}"), json!({"period": "{{p}}"})] {
        shared_cache().clear();
        let client = Arc::new(FakeFlowClient::new());
        client.reply("history", daily(20..=28));
        let c = ctx(client);
        c.vars.set("p", json!(3));
        answers.push(run(&c, "indicator", node(params)).await.value().cloned());
    }
    assert_eq!(
        answers[0].as_ref().map(|v| v["latest"].clone()),
        Some(json!({"value": 27.0}))
    );
    let object = answers[1].clone().unwrap_or(Value::Null);
    assert_eq!(object["status"], "error", "{}", object);
    assert!(object.get("latest").is_none(), "{}", object);
}

/// N-indicator-07, IND-074: the run log shows the lookback as set (500),
/// while the fetch is sized for the 200-bar ceiling; the node's source
/// reaches the fetch.
#[tokio::test]
async fn the_indicator_logs_its_lookback_as_set_and_fetches_its_source() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", daily(20..=29));
    let c = ctx(client.clone());
    run(
        &c,
        "indicator",
        json!({"symbol": "SBIN", "indicatorName": "sma", "params": "{\"period\": 2}",
               "lookbackBars": 500, "source": "db"}),
    )
    .await;
    assert!(
        logs(&c)
            .iter()
            .any(|(l, m)| l == "info" && m == "Computing sma for SBIN (D, 500 bars)"),
        "{:?}",
        logs(&c)
    );
    let calls = client.calls_to("history");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        Value::Object(calls[0].payload.clone()),
        json!({"symbol": "SBIN", "exchange": "NSE", "interval": "D", "source": "db",
               "start_date": "2025-12-08", "end_date": "2026-10-29"})
    );
}
