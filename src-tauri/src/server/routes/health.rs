//! Web `blueprints/health.py`. The web leaves `/health/status` and
//! `/health/check` open for load balancers; the desktop has none, so every
//! health route needs the signed-in user (CLAUDE.md, public route list).

use crate::db::sqlite::monitor as store;
use crate::server::envelope::json_response;
use crate::server::routes::webui::{csv_row, download, failed, ok};
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

fn latest(ctx: &AppState) -> crate::error::Result<Option<store::HealthRow>> {
    let c = ctx.logs.conn()?;
    store::latest_health(&c)
}

/// The latest sample for a status answer, which reports a read failure as
/// "unknown" instead of failing: the cause goes to the log here.
fn latest_for_status(ctx: &AppState) -> crate::error::Result<Option<store::HealthRow>> {
    let r = latest(ctx);
    if let Err(e) = &r {
        tracing::warn!("Reading the latest health sample failed: {}", e);
    }
    r
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
    let o = health::overall(&latest_for_status(&ctx), ctx.now());
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
    let main_ok = ctx
        .sqlite
        .conn()
        .and_then(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
        .is_ok();
    let logs_ok = ctx
        .logs
        .conn()
        .and_then(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
        .is_ok();
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
    let r = tokio::task::spawn_blocking(move || match latest(&c) {
        Ok(Some(m)) => Ok(Some(m)),
        Ok(None) => health::sample_once(&c).map(Some),
        Err(e) => Err(e),
    })
    .await;
    match r {
        Ok(Ok(Some(m))) => {
            let o = health::overall_of(&m, ctx.now());
            let mut v = with_freshness(health::current_json(&m), &o);
            if o.status == "unknown" {
                v["overall_status"] = json!("unknown");
            }
            ok(v)
        }
        Ok(Ok(None)) => json_response(
            StatusCode::NOT_FOUND,
            json!({"error": "No metrics available"}),
        ),
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

fn rows_since(ctx: &AppState, hours: i64) -> crate::error::Result<Vec<store::HealthRow>> {
    let c = ctx.logs.conn()?;
    store::health_since(&c, health::since(ctx.now(), hours))
}

/// GET /health/api/history?hours=
pub async fn history(State(ctx): Ctx, Query(q): Q) -> Response {
    let hours = health::clamp_hours(q.get("hours").map(|s| s.as_str()));
    match rows_since(&ctx, hours) {
        Ok(rows) => ok(health::history_json(&rows)),
        Err(e) => failed(
            "Health history",
            e,
            "Could not read health history. Try again.",
        ),
    }
}

/// GET /health/api/stats?hours=
pub async fn stats(State(ctx): Ctx, Query(q): Q) -> Response {
    let hours = health::clamp_hours(q.get("hours").map(|s| s.as_str()));
    match rows_since(&ctx, hours) {
        Ok(rows) => ok(health::stats_json(&rows, hours)),
        Err(e) => failed(
            "Health stats",
            e,
            "Could not read health statistics. Try again.",
        ),
    }
}

/// GET /health/api/alerts (unresolved, newest first)
pub async fn alerts(State(ctx): Ctx) -> Response {
    match ctx.logs.conn().and_then(|c| store::active_alerts(&c)) {
        Ok(rows) => ok(health::alerts_json(&rows)),
        Err(e) => failed("Health alerts", e, "Could not read alerts. Try again."),
    }
}

fn alert_action(ctx: &AppState, id: i64, resolve: bool) -> Response {
    let now = ctx.now();
    let r = ctx.logs.conn().and_then(|c| {
        if resolve {
            store::resolve_alert(&c, id, now)
        } else {
            store::acknowledge_alert(&c, id, now)
        }
    });
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

/// GET /health/export?hours=
pub async fn export(State(ctx): Ctx, Query(q): Q) -> Response {
    let hours = health::clamp_hours(q.get("hours").map(|s| s.as_str()));
    let rows = match rows_since(&ctx, hours) {
        Ok(r) => r,
        Err(e) => {
            return failed(
                "Health export",
                e,
                "Could not export health metrics. Try again.",
            )
        }
    };
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
    for m in &rows {
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
    download(out, "text/csv", "health_metrics.csv")
}
