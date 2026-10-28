//! Web `blueprints/traffic.py`: the request log written by the monitoring
//! middleware (`services::monitor`).

use crate::db::sqlite::monitor::TrafficRow;
use crate::server::routes::webui::{csv_row, download, failed, json_text, ok, ok_text};
use crate::services::security_service::web_time;
use crate::services::traffic_service::{self, r2};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    response::Response,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn row_json(t: &TrafficRow) -> Value {
    json!({
        "timestamp": web_time(&t.timestamp),
        "client_ip": t.client_ip,
        "method": t.method,
        "path": t.path,
        "status_code": t.status_code,
        "duration_ms": r2(t.duration_ms),
        "host": t.host,
        "error": t.error,
    })
}

/// GET /traffic/api/logs?limit= (newest first, at most 1000), read and
/// shaped on the blocking pool.
pub async fn logs(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let read = ctx.report(move |c| {
        let rows = traffic_service::recent(c, Some(limit))?;
        json_text(Value::Array(rows.iter().map(row_json).collect()))
    });
    match read.await {
        Ok(body) => ok_text(body),
        Err(e) => failed(
            "Reading traffic logs",
            e,
            "Could not load the traffic log. Try again.",
        ),
    }
}

/// GET /traffic/api/stats
pub async fn stats(State(ctx): Ctx) -> Response {
    match traffic_service::stats(&ctx).await {
        Ok(v) => ok(v),
        Err(e) => failed(
            "Traffic stats",
            e,
            "Could not load traffic statistics. Try again.",
        ),
    }
}

/// GET /traffic/export: every request as CSV, read and formatted on the
/// blocking pool.
pub async fn export(State(ctx): Ctx) -> Response {
    match traffic_service::export(&ctx, export_csv).await {
        Ok(out) => download(out, "text/csv", "traffic_logs.csv"),
        Err(e) => failed(
            "Exporting traffic logs",
            e,
            "Could not export the traffic log. Try again.",
        ),
    }
}

/// The web's CSV columns for the traffic log, one line per request; run on
/// the blocking pool (`traffic_service::export`).
fn export_csv(rows: &[TrafficRow]) -> String {
    let mut out = csv_row(
        &[
            "Timestamp",
            "Client IP",
            "Method",
            "Path",
            "Status Code",
            "Duration (ms)",
            "Host",
            "Error",
        ]
        .map(String::from),
    );
    for t in rows {
        out.push_str(&csv_row(&[
            web_time(&t.timestamp),
            t.client_ip.clone(),
            t.method.clone(),
            t.path.clone(),
            t.status_code.to_string(),
            r2(t.duration_ms).to_string(),
            t.host.clone().unwrap_or_default(),
            t.error.clone().unwrap_or_default(),
        ]));
    }
    out
}
