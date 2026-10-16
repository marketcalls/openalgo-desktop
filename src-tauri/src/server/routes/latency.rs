//! Web `blueprints/latency.py`: `/api/v1` call latency recorded by the
//! monitoring middleware (`services::monitor`). The statistics are
//! `latency_service`.

use crate::db::sqlite::monitor::LatencyRow;
use crate::server::envelope::json_response;
use crate::server::routes::webui::{csv_row, download_stream, failed, ok};
use crate::services::latency_service;
use crate::services::security_service::web_time;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn row_json(l: &LatencyRow) -> Value {
    json!({
        "timestamp": crate::services::health_service::iso_ist(&l.timestamp),
        "id": l.id,
        "order_id": l.order_id,
        "broker": l.broker,
        "symbol": l.symbol,
        "order_type": l.order_type,
        "rtt_ms": l.rtt_ms,
        "validation_latency_ms": l.validation_latency_ms,
        "response_latency_ms": l.response_latency_ms,
        "overhead_ms": l.overhead_ms,
        "total_latency_ms": l.total_latency_ms,
        "status": l.status,
        "error": l.error,
    })
}

/// GET /latency/api/logs?limit=
pub async fn logs(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    match latency_service::recent(&ctx, limit).await {
        Ok(rows) => ok(Value::Array(rows.iter().map(row_json).collect())),
        Err(e) => failed(
            "Reading latency logs",
            e,
            "Could not load the latency log. Try again.",
        ),
    }
}

/// GET /latency/api/stats
pub async fn stats(State(ctx): Ctx) -> Response {
    match latency_service::stats(&ctx).await {
        Ok(v) => ok(v),
        Err(e) => failed(
            "Latency stats",
            e,
            "Could not load latency statistics. Try again.",
        ),
    }
}

/// GET /latency/api/broker/{broker}/stats
pub async fn broker_stats(State(ctx): Ctx, Path(broker): Path<String>) -> Response {
    let v = match latency_service::stats(&ctx).await {
        Ok(v) => v,
        Err(e) => {
            return failed(
                "Latency stats",
                e,
                "Could not load latency statistics. Try again.",
            )
        }
    };
    match v["broker_stats"].get(&broker) {
        Some(Value::Object(m)) => {
            let mut m = m.clone();
            m.insert("histogram".into(), v["broker_histograms"][&broker].clone());
            ok(Value::Object(m))
        }
        _ => json_response(StatusCode::NOT_FOUND, json!({"error": "Broker not found"})),
    }
}

/// The export's columns (web `generate_csv`).
const EXPORT_HEADER: [&str; 10] = [
    "Date & Time (IST)",
    "Broker",
    "Order ID",
    "Symbol",
    "Order Type",
    "Broker Confirmation (ms)",
    "Platform Overhead (ms)",
    "Total Latency (ms)",
    "Status",
    "Error (if any)",
];

fn export_line(l: &LatencyRow) -> String {
    let r2 = |v: f64| ((v * 100.0).round() / 100.0).to_string();
    csv_row(&[
        web_time(&l.timestamp),
        l.broker.clone().unwrap_or_else(|| "N/A".into()),
        l.order_id.clone(),
        l.symbol.clone().unwrap_or_else(|| "N/A".into()),
        l.order_type.clone(),
        r2(l.rtt_ms),
        r2(l.overhead_ms),
        r2(l.total_latency_ms),
        l.status.clone(),
        l.error.clone().unwrap_or_default(),
    ])
}

/// GET /latency/export: every call, streamed a page at a time (DB-03).
pub async fn export(State(ctx): Ctx) -> Response {
    let header = csv_row(&EXPORT_HEADER.map(String::from));
    let rows = latency_service::export_pages(ctx)
        .map_ok(|page| page.iter().map(export_line).collect::<String>());
    download_stream(
        stream::once(async { Ok(header) }).chain(rows),
        "text/csv",
        "latency_logs.csv",
    )
}
