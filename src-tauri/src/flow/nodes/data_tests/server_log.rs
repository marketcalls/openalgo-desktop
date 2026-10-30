//! The History node's server-log lines (web `indicator_service`
//! `clamp_history_range` and `trim_records`, logged at info): a range past
//! the 200-bar ceiling and an answer longer than it. The run log does not
//! carry them, on the web either; the server log does (decision 35).

use super::support::{ctx, logs, run, ServerLog};
use crate::flow::contract::client::fake::FakeFlowClient;
use serde_json::{json, Value};
use std::sync::Arc;

/// N-history-02: a range wider than 200 bars of its interval starts at the
/// ceiling's floor instead, and the server log says so.
#[tokio::test]
async fn a_range_past_the_ceiling_is_narrowed_and_logged() {
    let log = ServerLog::capture();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", json!({"status": "success", "data": []}));
    let c = ctx(client.clone());
    run(
        &c,
        "history",
        json!({"symbol": "SBIN", "interval": "5m", "startDate": "2020-01-01", "endDate": "2026-10-28"}),
    )
    .await;
    let p = client.calls()[0].payload.clone();
    assert_eq!(
        (p["start_date"].clone(), p["end_date"].clone()),
        (json!("2026-10-19"), json!("2026-10-28"))
    );
    assert!(
        log.has(
            "INFO",
            "History range 2020-01-01..2026-10-28 at 5m exceeds the 200-bar ceiling; starting from 2026-10-19 instead"
        ),
        "{:?}",
        log.lines()
    );
    assert!(!logs(&c).iter().any(|(_, m)| m.contains("ceiling")));
}

/// N-history-03: an answer of more than 200 bars keeps the latest 200, and
/// the server log says how many came back.
#[tokio::test]
async fn a_long_answer_is_trimmed_and_logged() {
    let log = ServerLog::capture();
    let client = Arc::new(FakeFlowClient::new());
    let bars: Vec<Value> = (0..250)
        .map(|i| json!({"timestamp": 1_792_000_000 + i * 60, "close": i}))
        .collect();
    client.reply("history", json!({"status": "success", "data": bars}));
    let c = ctx(client);
    let out = run(
        &c,
        "history",
        json!({"symbol": "SBIN", "interval": "D", "startDate": "2026-10-01", "endDate": "2026-10-28"}),
    )
    .await;
    let data = out
        .value()
        .and_then(|v| v["data"].as_array().cloned())
        .unwrap_or_default();
    assert_eq!(data.len(), 200);
    assert_eq!(
        (data[0]["close"].clone(), data[199]["close"].clone()),
        (json!(50), json!(249))
    );
    assert!(
        log.has(
            "INFO",
            "History returned 250 bars; trimming to the latest 200"
        ),
        "{:?}",
        log.lines()
    );
}
