//! The request log the monitoring middleware writes (`services::monitor`),
//! read for the Traffic page (web `blueprints/traffic.py`). Every read
//! writes out what is queued first, so the page includes the latest
//! requests.

use crate::db::sqlite::monitor::{self as store, TrafficRow};
use crate::error::Result;
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The endpoints the web's stats break out.
const ENDPOINTS: &[&str] = &[
    "placeorder",
    "placesmartorder",
    "modifyorder",
    "cancelorder",
    "quotes",
    "history",
    "depth",
    "intervals",
    "funds",
    "orderbook",
    "tradebook",
    "positionbook",
    "holdings",
    "basketorder",
    "splitorder",
    "orderstatus",
    "openposition",
];

/// Rounded to two decimals, as the web shows durations.
pub fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// The newest requests first, at most `limit` (`None`: all of them).
pub fn recent(ctx: &AppState, limit: Option<i64>) -> Result<Vec<TrafficRow>> {
    ctx.monitor.drain_now(ctx);
    let c = ctx.logs.conn()?;
    store::recent_traffic(&c, limit)
}

/// Every request, newest first, turned into text by `render`, both off the
/// async workers: the rows are read, the connection returned, then
/// rendered. On the async worker a full log (100,000 requests) held the
/// runtime for seconds.
pub async fn export<F>(ctx: &Arc<AppState>, render: F) -> Result<String>
where
    F: FnOnce(&[TrafficRow]) -> String + Send + 'static,
{
    ctx.report(move |c| Ok(render(&recent(c, None)?))).await
}

/// `GET /traffic/api/stats`: totals, errors and average duration overall,
/// for `/api/v1` and per endpoint, read off the async workers (19 scans
/// of a log of up to 100,000 requests). A refresh that arrives while the
/// statistics are computed waits for them and answers with them
/// (`ReportFlight`).
pub async fn stats(ctx: &Arc<AppState>) -> Result<Value> {
    ctx.traffic_reports.share(ctx, stats_now).await
}

fn stats_now(ctx: &AppState) -> Result<Value> {
    ctx.monitor.drain_now(ctx);
    let (all, api, endpoints) = {
        let c = ctx.logs.conn()?;
        let all = store::traffic_summary(&c, None)?;
        let api = store::traffic_summary(&c, Some("/api/v1/%"))?;
        let mut endpoints = Map::new();
        for e in ENDPOINTS {
            let (t, err, avg) = store::traffic_summary(&c, Some(&format!("/api/v1/{}%", e)))?;
            endpoints.insert(
                e.to_string(),
                json!({"total": t, "errors": err, "avg_duration": r2(avg)}),
            );
        }
        (all, api, endpoints)
    };
    Ok(json!({
        "overall": {"total_requests": all.0, "error_requests": all.1, "avg_duration": r2(all.2)},
        "api": {"total_requests": api.0, "error_requests": api.1, "avg_duration": r2(api.2)},
        "endpoints": endpoints,
    }))
}
