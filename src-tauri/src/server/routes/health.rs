//! Web `blueprints/health.py`. The web leaves `/health/status` and
//! `/health/check` open for load balancers; the desktop has none, so every
//! health route needs the signed-in user (CLAUDE.md, public route list).

use crate::db::sqlite::monitor as store;
use crate::server::envelope::json_response;
use crate::server::routes::webui::{csv_row, download, failed, json_text, ok, ok_text};
use crate::services::health_service as health;
use crate::services::security_service::web_time;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

/// The latest sample for a status answer, which reports a read failure as
/// "unknown" instead of failing: the cause goes to the log here.
fn latest_for_status(ctx: &AppState) -> crate::error::Result<Option<store::HealthRow>> {
    let r = health::latest_sample(ctx);
    if let Err(e) = &r {
        tracing::warn!("Reading the latest health sample failed: {}", e);
    }
    r
}

/// The overall status, "fail" while a gap in order facts is open: management
/// is degraded right now whatever the last sample says (EV-03).
fn overall(ctx: &AppState) -> health::Overall {
    let mut o = health::overall(&latest_for_status(ctx), ctx.now());
    if health::order_facts_failing(ctx) {
        o.status = "fail".into();
        o.reason = None;
    }
    o
}

/// Adds the sample's age, and the reason when the status is "unknown".
fn with_freshness(mut body: Value, o: &health::Overall) -> Value {
    body["sample_age_s"] = json!(o.sample_age_s);
    if let Some(r) = &o.reason {
        body["reason"] = json!(r);
    }
    body
}

/// GET /health and /health/status: `{status, version, serviceId, description}`
/// plus `sample_age_s`, and `reason` when the status is "unknown" (no sample,
/// an unreadable one, or one older than two sampling periods). Only "fail"
/// answers 503.
pub async fn status(State(ctx): Ctx) -> Response {
    let o = overall(&ctx);
    let code = if o.status == "fail" {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    json_response(
        code,
        with_freshness(
            json!({"status": o.status, "version": "1.0", "serviceId": "openalgo", "description": "OpenAlgo Trading Platform"}),
            &o,
        ),
    )
}

/// GET /health/check: database reachability plus the latest sample.
pub async fn check(State(ctx): Ctx) -> Response {
    let now = ctx.now().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string();
    let (main_ok, logs_ok) = health::databases_answer(&ctx);
    let pf = |b: bool| if b { "pass" } else { "fail" };
    let mut checks = serde_json::Map::new();
    checks.insert(
        "database:connectivity".into(),
        json!([
            {"componentId": "openalgo", "status": pf(main_ok), "time": now},
            {"componentId": "logs", "status": pf(logs_ok), "time": now},
        ]),
    );
    let sample = latest_for_status(&ctx);
    let o = health::overall(&sample, ctx.now());
    let m = sample.ok().flatten();
    if let Some(m) = &m {
        let t = format!("{}Z", m.timestamp.replace(' ', "T"));
        if let Some(fd) = m.fd_count {
            checks.insert(
                "system:file-descriptors".into(),
                json!([{"componentId": "fd_count", "status": m.fd_status.clone().unwrap_or_else(|| "pass".into()),
                        "observedValue": fd, "observedUnit": "count", "time": t}]),
            );
        }
        if let Some(rss) = m.memory_rss_mb {
            checks.insert(
                "system:memory".into(),
                json!([{"componentId": "rss", "status": m.memory_status.clone().unwrap_or_else(|| "pass".into()),
                        "observedValue": (rss * 100.0).round() / 100.0, "observedUnit": "MiB", "time": t}]),
            );
        }
    }
    // A database that does not answer is a failure whatever the sample says;
    // otherwise the sample decides, "unknown" when it is missing or stale.
    let st = if !(main_ok && logs_ok) {
        "fail"
    } else {
        o.status.as_str()
    };
    json_response(
        if st == "fail" {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        },
        with_freshness(
            json!({"status": st, "version": "1.0", "serviceId": "openalgo",
                   "description": "OpenAlgo Trading Platform", "checks": checks}),
            &o,
        ),
    )
}

/// GET /health/api/current (takes a sample first if none exists yet), plus
/// `sample_age_s`; a stale sample reports `overall_status` "unknown" with a
/// `reason`, its own readings unchanged.
pub async fn current(State(ctx): Ctx) -> Response {
    let c = ctx.clone();
    let r = tokio::task::spawn_blocking(move || health::current_sample(&c)).await;
    match r {
        Ok(Ok((m, growth))) => {
            let o = health::overall_of(&m, ctx.now());
            let mut v = with_freshness(health::current_json(&m), &o);
            v["database"]["growth"] = growth;
            v["order_facts"] = health::order_facts_json(&ctx);
            v["records"] = crate::services::record_loss::json(&ctx);
            v["strategy_ticks"] = crate::services::strategy_ticks::json(&ctx);
            v["market_data"] = crate::services::feed_losses::json(&ctx);
            if health::order_facts_failing(&ctx) {
                v["overall_status"] = json!("fail");
            } else if o.status == "unknown" {
                v["overall_status"] = json!("unknown");
            }
            ok(v)
        }
        Ok(Err(e)) => failed(
            "Health metrics",
            e,
            "Could not read health metrics. Try again.",
        ),
        Err(e) => failed(
            "Health metrics",
            e,
            "Could not read health metrics. Try again.",
        ),
    }
}

/// GET /health/api/history?hours=, read, shaped and serialized on the
/// blocking pool (a week is 10,080 samples).
pub async fn history(State(ctx): Ctx, Query(q): Q) -> Response {
    let hours = health::clamp_hours(q.get("hours").map(|s| s.as_str()));
    let read =
        ctx.report(move |c| json_text(health::history_json(&health::samples_since(c, hours)?)));
    match read.await {
        Ok(body) => ok_text(body),
        Err(e) => failed(
            "Health history",
            e,
            "Could not read health history. Try again.",
        ),
    }
}

/// GET /health/api/stats?hours=, read and summed on the blocking pool.
pub async fn stats(State(ctx): Ctx, Query(q): Q) -> Response {
    let hours = health::clamp_hours(q.get("hours").map(|s| s.as_str()));
    let read =
        ctx.report(move |c| Ok(health::stats_json(&health::samples_since(c, hours)?, hours)));
    match read.await {
        Ok(v) => ok(v),
        Err(e) => failed(
            "Health stats",
            e,
            "Could not read health statistics. Try again.",
        ),
    }
}

/// GET /health/api/alerts (unresolved, newest first), read on the blocking
/// pool.
pub async fn alerts(State(ctx): Ctx) -> Response {
    let read = ctx.blocking(|c| Ok(health::alerts_json(&health::active_alerts(c)?)));
    match read.await {
        Ok(v) => ok(v),
        Err(e) => failed("Health alerts", e, "Could not read alerts. Try again."),
    }
}

fn alert_action(ctx: &AppState, id: i64, resolve: bool) -> Response {
    let r = health::settle_alert(ctx, id, resolve);
    let done = if resolve {
        "Alert resolved"
    } else {
        "Alert acknowledged"
    };
    match r {
        Ok(true) => ok(json!({"status": "success", "message": done})),
        Ok(false) => crate::server::envelope::error(StatusCode::NOT_FOUND, "Alert not found"),
        Err(e) => failed(
            "Updating an alert",
            e,
            "Could not update the alert. Try again.",
        ),
    }
}

/// POST /health/api/alerts/{alert_id}/acknowledge
pub async fn acknowledge(State(ctx): Ctx, Path(id): Path<i64>) -> Response {
    alert_action(&ctx, id, false)
}

/// POST /health/api/alerts/{alert_id}/resolve
pub async fn resolve(State(ctx): Ctx, Path(id): Path<i64>) -> Response {
    alert_action(&ctx, id, true)
}

/// GET /health/api/held-orders: Live orders the broker did not confirm and
/// does not show, held for the trader's decision (P0-3).
pub async fn held_orders(State(ctx): Ctx) -> Response {
    let rows: Vec<serde_json::Value> = ctx
        .reconciler
        .held_orders()
        .into_iter()
        .map(|h| {
            json!({
                "owner": h.owner,
                "id": h.id,
                "symbol": h.symbol,
                "exchange": h.exchange,
                "action": h.action,
                "quantity": h.quantity,
                "since": h.since.with_timezone(&chrono_tz::Asia::Kolkata).format("%Y-%m-%d %H:%M:%S").to_string(),
                "message": h.message,
            })
        })
        .collect();
    ok(json!({"status": "success", "data": rows}))
}

#[derive(serde::Deserialize)]
pub struct NotPlaced {
    owner: String,
    id: String,
}

/// POST /health/api/held-orders/not-placed `{owner, id}`: the trader checked
/// the broker's books and the order is not there; release it and resume.
pub async fn held_not_placed(State(ctx): Ctx, axum::Json(body): axum::Json<NotPlaced>) -> Response {
    match ctx
        .reconciler
        .confirm_not_placed(body.owner.trim(), body.id.trim())
        .await
    {
        Ok(()) => ok(json!({
            "status": "success",
            "message": "Marked as not placed. The position is managed again."
        })),
        Err(msg) => crate::server::envelope::error(StatusCode::BAD_REQUEST, msg),
    }
}

/// GET /health/export?hours=: the samples as CSV, read and formatted on the
/// blocking pool.
pub async fn export(State(ctx): Ctx, Query(q): Q) -> Response {
    let hours = health::clamp_hours(q.get("hours").map(|s| s.as_str()));
    match ctx
        .report(move |c| Ok(export_csv(&health::samples_since(c, hours)?)))
        .await
    {
        Ok(out) => download(out, "text/csv", "health_metrics.csv"),
        Err(e) => failed(
            "Health export",
            e,
            "Could not export health metrics. Try again.",
        ),
    }
}

/// The web's CSV columns for the health samples, one line per sample; run
/// on the blocking pool (`export`).
fn export_csv(rows: &[store::HealthRow]) -> String {
    let mut out = csv_row(
        &[
            "Date & Time (IST)",
            "FD Count",
            "FD Limit",
            "FD Status",
            "Memory (MB)",
            "Memory Status",
            "DB Connections",
            "DB Status",
            "WebSocket Connections",
            "WS Status",
            "Threads",
            "Thread Status",
            "Overall Status",
        ]
        .map(String::from),
    );
    let s = |v: &Option<String>| v.clone().unwrap_or_else(|| "unknown".into());
    let n = |v: Option<i64>| v.unwrap_or(0).to_string();
    for m in rows {
        out.push_str(&csv_row(&[
            web_time(&m.timestamp),
            n(m.fd_count),
            n(m.fd_limit),
            s(&m.fd_status),
            m.memory_rss_mb
                .map(|v| ((v * 100.0).round() / 100.0).to_string())
                .unwrap_or_else(|| "0".into()),
            s(&m.memory_status),
            n(m.db_connections_total),
            s(&m.db_status),
            n(m.ws_connections_total),
            s(&m.ws_status),
            n(m.thread_count),
            s(&m.thread_status),
            s(&m.overall_status),
        ]));
    }
    out
}
