//! Web `blueprints/latency.py`: `/api/v1` call latency recorded by the
//! monitoring middleware (`services::monitor`). The statistics are
//! `latency_service`.

use crate::db::sqlite::monitor::{latency_groups as groups, LatencyRow};
use crate::server::envelope::{json_response, EarlyReply};
use crate::server::routes::webui::{csv_row, download_stream, failed, ok};
use crate::services::latency_service;
use crate::services::security_service::web_time;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

type Q = Query<HashMap<String, String>>;

/// A record as the page reads it: with the name and group of its endpoint,
/// so the page keeps no table of its own (web `log_to_dict`).
fn row_json(l: &LatencyRow) -> Value {
    let tag = Some(l.order_type.as_str()).filter(|t| !t.is_empty());
    json!({
        "timestamp": crate::services::health_service::iso_ist(&l.timestamp),
        "id": l.id,
        "order_id": l.order_id,
        "broker": l.broker,
        "symbol": l.symbol,
        "order_type": tag,
        "endpoint": groups::label_of(tag),
        "group": groups::group_key_of(tag),
        "rtt_ms": l.rtt_ms,
        "validation_latency_ms": l.validation_latency_ms,
        "response_latency_ms": l.response_latency_ms,
        "overhead_ms": l.overhead_ms,
        "total_latency_ms": l.total_latency_ms,
        "status": l.status,
        "error": l.error,
    })
}

fn refused(message: String) -> Response {
    json_response(StatusCode::BAD_REQUEST, json!({"error": message}))
}

/// The group the request names, or its 400 (web `requested_group`).
fn group(q: &HashMap<String, String>, default: &'static str) -> Result<&'static str, EarlyReply> {
    groups::requested(q.get("group").map(String::as_str), default).map_err(|m| Box::new(refused(m)))
}

/// GET /latency/api/logs?group=&limit=
pub async fn logs(State(ctx): Ctx, Query(q): Q) -> Response {
    let group = match group(&q, groups::DEFAULT_GROUP) {
        Ok(g) => g,
        Err(r) => return *r,
    };
    let limit = match q.get("limit").map(|l| l.trim().parse::<i64>()) {
        None => 100,
        Some(Ok(l)) => l.clamp(1, 1000),
        Some(Err(_)) => return refused("The limit has to be a whole number.".into()),
    };
    match latency_service::recent(&ctx, group, limit).await {
        Ok(rows) => ok(Value::Array(rows.iter().map(row_json).collect())),
        Err(e) => failed(
            "Reading latency logs",
            e,
            "The latency records could not be read. Try again.",
        ),
    }
}

/// GET /latency/api/stats?group=
pub async fn stats(State(ctx): Ctx, Query(q): Q) -> Response {
    let group = match group(&q, groups::DEFAULT_GROUP) {
        Ok(g) => g,
        Err(r) => return *r,
    };
    match latency_service::stats(&ctx, group).await {
        Ok(v) => ok(v),
        Err(e) => failed(
            "Latency stats",
            e,
            "The latency figures could not be read. Try again.",
        ),
    }
}

/// GET /latency/api/broker/{broker}/stats?group=: the broker's figures in
/// the group, and the histogram of all its records.
pub async fn broker_stats(State(ctx): Ctx, Path(broker): Path<String>, Query(q): Q) -> Response {
    let group = match group(&q, groups::DEFAULT_GROUP) {
        Ok(g) => g,
        Err(r) => return *r,
    };
    match latency_service::broker_page(&ctx, group, broker).await {
        Ok(Some(row)) => ok(Value::Object(row)),
        Ok(None) => json_response(StatusCode::NOT_FOUND, json!({"error": "Broker not found"})),
        Err(e) => failed(
            "Latency stats",
            e,
            "The latency figures could not be read. Try again.",
        ),
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

/// GET /latency/export?group=: every call of the group on screen, or every
/// call when none is named, streamed a page at a time and each page
/// formatted on the blocking pool with its read (DB-03).
pub async fn export(State(ctx): Ctx, Query(q): Q) -> Response {
    let group = match group(&q, groups::ALL) {
        Ok(g) => g,
        Err(r) => return *r,
    };
    let header = csv_row(&EXPORT_HEADER.map(String::from));
    let rows = latency_service::export_pages(ctx, group, |page| {
        page.iter().map(export_line).collect::<String>()
    });
    let filename = match group {
        groups::ALL => "latency_logs.csv".to_string(),
        g => format!("latency_{g}.csv"),
    };
    download_stream(
        stream::once(async { Ok(header) }).chain(rows),
        "text/csv",
        &filename,
    )
}
