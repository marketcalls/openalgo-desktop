//! Health monitor (web `utils/health_monitor.py`, `database/health_db.py`).
//!
//! A sampler owned by the app context records, once a minute, the process's
//! open file descriptors (handles on Windows), resident memory, thread count,
//! database pool connections and database file sizes into `health_metrics`
//! (bounded: 7 days and a row cap, see `monitor::RETENTION`). Crossing a
//! level raises an alert; dropping back under the warning level resolves it.
//! This is the app's own leak monitor: a line that only goes up in
//! `/health/api/history` is a leak.

use crate::db::sqlite::monitor::{self as store, AlertRow, HealthRow};
use crate::error::Result;
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const SAMPLE_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// (warning, critical) levels. Descriptors on Windows are kernel handles,
/// which a healthy process holds many more of.
pub fn fd_levels() -> (i64, i64) {
    if cfg!(windows) {
        (5_000, 10_000)
    } else {
        (700, 900)
    }
}
pub const MEMORY_LEVELS_MB: (f64, f64) = (500.0, 1_000.0);
pub const DB_LEVELS: (i64, i64) = (10, 20);
pub const WS_LEVELS: (i64, i64) = (10, 20);
/// The async runtime keeps a worker per core plus a blocking pool, so the
/// thread budget is higher than the web's eventlet numbers.
pub const THREAD_LEVELS: (i64, i64) = (200, 400);

#[derive(Debug, Clone, Default)]
pub struct ProcessStats {
    pub fd_count: Option<i64>,
    pub fd_limit: Option<i64>,
    pub rss_mb: Option<f64>,
    pub vms_mb: Option<f64>,
    pub memory_percent: Option<f64>,
    pub available_mb: Option<f64>,
    pub swap_mb: Option<f64>,
    pub threads: Option<i64>,
}

#[cfg(target_os = "macos")]
fn thread_count() -> Option<i64> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, a
    // correctly sized, initialised proc_taskinfo owned by this frame.
    let n = unsafe {
        libc::proc_pidinfo(
            std::process::id() as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    (n == size).then_some(info.pti_threadnum as i64)
}

#[cfg(target_os = "linux")]
fn thread_count() -> Option<i64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("Threads:"))
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn thread_count() -> Option<i64> {
    None
}

/// Read this process's resource use.
pub fn process_stats() -> ProcessStats {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_memory();
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_memory(),
    );
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    let total = sys.total_memory();
    let mut s = ProcessStats {
        available_mb: Some(mb(sys.available_memory())),
        swap_mb: Some(mb(sys.used_swap())),
        threads: thread_count(),
        ..Default::default()
    };
    if let Some(p) = sys.process(pid) {
        s.rss_mb = Some(mb(p.memory()));
        s.vms_mb = Some(mb(p.virtual_memory()));
        s.memory_percent = (total > 0).then(|| p.memory() as f64 / total as f64 * 100.0);
        s.fd_count = p.open_files().map(|n| n as i64);
        s.fd_limit = p.open_files_limit().map(|n| n as i64);
    }
    s
}

fn status_of(v: Option<f64>, (warn, fail): (f64, f64)) -> &'static str {
    match v {
        None => "unknown",
        Some(x) if x >= fail => "fail",
        Some(x) if x >= warn => "warn",
        Some(_) => "pass",
    }
}

fn worst<'a>(statuses: &[&'a str]) -> &'a str {
    if statuses.contains(&"fail") {
        "fail"
    } else if statuses.contains(&"warn") {
        "warn"
    } else {
        "pass"
    }
}

/// File sizes of each database, MB (including WAL).
pub fn db_sizes(ctx: &AppState) -> Map<String, Value> {
    let mut m = Map::new();
    for (name, file) in [
        ("openalgo", "openalgo.db"),
        ("logs", "logs.db"),
        ("historify", "historify.duckdb"),
    ] {
        let base = ctx.data_dir.join(file);
        let mut bytes = 0u64;
        for suffix in ["", "-wal", "-shm"] {
            let p = std::path::PathBuf::from(format!("{}{}", base.display(), suffix));
            if let Ok(md) = std::fs::metadata(&p) {
                bytes += md.len();
            }
        }
        m.insert(
            name.into(),
            json!((bytes as f64 / (1024.0 * 1024.0) * 100.0).round() / 100.0),
        );
    }
    m
}

/// Take one sample, store it, raise or resolve alerts. Blocking: run it on a
/// blocking thread.
pub fn sample_once(ctx: &AppState) -> Result<HealthRow> {
    let now = ctx.now();
    let p = process_stats();
    let (main_conns, _) = ctx.sqlite.pool_state();
    let (logs_conns, _) = ctx.logs.pool_state();
    let db_total = (main_conns + logs_conns) as i64;
    let mut ws = Map::new();
    let symbols = ctx.websocket.instrument_count() as i64;
    let ws_total = if ctx.websocket.is_connected() {
        let broker = ctx
            .get_broker_session()
            .map(|b| b.broker_id)
            .unwrap_or_else(|| "broker".into());
        ws.insert(broker, json!({"count": 1, "symbols": symbols}));
        1
    } else {
        0
    };
    let fd_levels = fd_levels();
    let fd_status = status_of(
        p.fd_count.map(|v| v as f64),
        (fd_levels.0 as f64, fd_levels.1 as f64),
    );
    let mem_status = status_of(p.rss_mb, MEMORY_LEVELS_MB);
    let db_status = status_of(
        Some(db_total as f64),
        (DB_LEVELS.0 as f64, DB_LEVELS.1 as f64),
    );
    let ws_status = status_of(
        Some(ws_total as f64),
        (WS_LEVELS.0 as f64, WS_LEVELS.1 as f64),
    );
    let thread_status = status_of(
        p.threads.map(|v| v as f64),
        (THREAD_LEVELS.0 as f64, THREAD_LEVELS.1 as f64),
    );
    let overall = worst(&[fd_status, mem_status, db_status, ws_status, thread_status]);
    let row = HealthRow {
        timestamp: store::ts(now),
        fd_count: p.fd_count,
        fd_limit: p.fd_limit,
        fd_usage_percent: match (p.fd_count, p.fd_limit) {
            (Some(c), Some(l)) if l > 0 => Some((c as f64 / l as f64 * 10_000.0).round() / 100.0),
            _ => None,
        },
        fd_status: Some(fd_status.into()),
        memory_rss_mb: p.rss_mb,
        memory_vms_mb: p.vms_mb,
        memory_percent: p.memory_percent,
        memory_available_mb: p.available_mb,
        memory_swap_mb: p.swap_mb,
        memory_status: Some(mem_status.into()),
        db_connections_total: Some(db_total),
        db_connections: Some(json!({"openalgo": main_conns, "logs": logs_conns})),
        db_status: Some(db_status.into()),
        ws_connections_total: Some(ws_total),
        ws_connections: Some(Value::Object(ws)),
        ws_total_symbols: Some(symbols),
        ws_status: Some(ws_status.into()),
        thread_count: p.threads,
        stuck_threads: Some(0),
        thread_details: Some(json!([])),
        thread_status: Some(thread_status.into()),
        process_details: Some(json!([{
            "pid": std::process::id(),
            "name": "OpenAlgo Desktop",
            "rss_mb": p.rss_mb.unwrap_or(0.0),
            "vms_mb": p.vms_mb.unwrap_or(0.0),
            "memory_percent": p.memory_percent.unwrap_or(0.0),
        }])),
        db_sizes: Some(Value::Object(db_sizes(ctx))),
        overall_status: Some(overall.into()),
        ..Default::default()
    };
    let conn = ctx.logs.conn()?;
    let id = store::insert_health(&conn, &row)?;
    alerts(
        &conn,
        now,
        "fd_count",
        "Open files",
        p.fd_count.map(|v| v as f64),
        (fd_levels.0 as f64, fd_levels.1 as f64),
        "",
    )?;
    alerts(
        &conn,
        now,
        "memory_rss_mb",
        "Memory use",
        p.rss_mb,
        MEMORY_LEVELS_MB,
        " MB",
    )?;
    alerts(
        &conn,
        now,
        "db_connections_total",
        "Database connections",
        Some(db_total as f64),
        (DB_LEVELS.0 as f64, DB_LEVELS.1 as f64),
        "",
    )?;
    alerts(
        &conn,
        now,
        "thread_count",
        "Threads",
        p.threads.map(|v| v as f64),
        (THREAD_LEVELS.0 as f64, THREAD_LEVELS.1 as f64),
        "",
    )?;
    Ok(HealthRow { id, ..row })
}

fn alerts(
    conn: &rusqlite::Connection,
    now: DateTime<Utc>,
    metric: &str,
    label: &str,
    value: Option<f64>,
    (warn, fail): (f64, f64),
    unit: &str,
) -> Result<()> {
    let Some(v) = value else {
        return Ok(());
    };
    let (sev, level, word) = if v >= fail {
        ("fail", fail, "critical")
    } else if v >= warn {
        ("warn", warn, "high")
    } else {
        store::auto_resolve(conn, metric, now)?;
        return Ok(());
    };
    store::raise_alert(
        conn,
        &AlertRow {
            alert_type: format!("{}_{}", metric, sev),
            severity: sev.into(),
            metric_name: metric.into(),
            metric_value: v,
            threshold_value: level,
            message: format!(
                "{} is {}: {:.0}{} (level {:.0}{}). If it keeps rising, restart OpenAlgo and report it.",
                label, word, v, unit, level, unit
            ),
            ..Default::default()
        },
        now,
    )
}

/// Start the once-a-minute sampler (owned by the context, stops on shutdown).
pub fn start(ctx: &Arc<AppState>) {
    let c = ctx.clone();
    let token = ctx.shutdown.child_token();
    ctx.spawn(async move {
        let mut tick = tokio::time::interval(SAMPLE_EVERY);
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tick.tick() => {
                    let c2 = c.clone();
                    let r = tokio::task::spawn_blocking(move || {
                        let r = sample_once(&c2);
                        crate::services::monitor::housekeep(&c2);
                        r
                    })
                    .await;
                    if let Ok(Err(e)) = r {
                        tracing::warn!("Health sample failed: {}", e);
                    }
                }
            }
        }
    });
}

// ------------------------------------------------------------ order facts

/// Alert type and metric of a possible loss of order facts (EV-03).
pub const ORDER_FACTS_ALERT: &str = "order_facts_gap";
pub const ORDER_FACTS_METRIC: &str = "order_facts";

/// The trader-facing alert text for a possible loss of order facts.
pub fn order_facts_message(cause: crate::services::order_reconciler::GapCause) -> String {
    use crate::services::order_reconciler::GapCause;
    let why = match cause {
        GapCause::RelayLagged => "the broker's order updates arrived faster than OpenAlgo could relay them",
        GapCause::EventLoss => "an order update could not be delivered inside OpenAlgo",
        GapCause::OrderFeedReconnected => "the broker's order feed reconnected",
    };
    format!(
        "Some order updates may have been missed: {}. OpenAlgo is re-checking the broker's order book, and new strategy entries wait until it has; stops and exits keep running.",
        why
    )
}

/// Raise or resolve the order-facts alert (the reconciler's sink).
pub fn order_facts_alert(ctx: &AppState, a: crate::services::order_reconciler::FactAlert) {
    use crate::services::order_reconciler::FactAlert;
    let now = ctx.now();
    let r = ctx.logs.conn().and_then(|c| match a {
        FactAlert::Raised(cause) => store::raise_alert(
            &c,
            &AlertRow {
                alert_type: ORDER_FACTS_ALERT.into(),
                severity: "fail".into(),
                metric_name: ORDER_FACTS_METRIC.into(),
                metric_value: 1.0,
                message: order_facts_message(cause),
                ..Default::default()
            },
            now,
        ),
        FactAlert::Resolved => store::auto_resolve(&c, ORDER_FACTS_METRIC, now).map(|_| ()),
    });
    if let Err(e) = r {
        tracing::warn!("Order-facts alert not recorded: {}", e);
    }
}

/// Order-fact health (EV-01, EV-03): the event bus's loss counters, the
/// depth of every critical queue, and the order reconciler's open gaps.
/// `status` is `fail` while a gap is open: management is degraded.
pub fn order_facts_json(ctx: &AppState) -> Value {
    use std::sync::atomic::Ordering::Relaxed;
    let b = ctx.bus.stats();
    let mut v = ctx.reconciler.health();
    v["bus"] = json!({
        "published": b.published.load(Relaxed),
        "dropped": b.dropped.load(Relaxed),
        "critical_dropped": b.critical_dropped.load(Relaxed),
        "closed_sends": b.closed_sends.load(Relaxed),
        "panics": b.panics.load(Relaxed),
        "critical_panics": b.critical_panics.load(Relaxed),
        "stale_dropped": b.stale_dropped.load(Relaxed),
        "relay_lagged": b.relay_lagged.load(Relaxed),
    });
    v["critical_queues"] = Value::Array(
        ctx.bus
            .queue_depths()
            .into_iter()
            .filter(|q| q.lane == crate::events::Lane::Critical)
            .map(|q| json!({"name": q.name, "depth": q.depth, "capacity": q.capacity}))
            .collect(),
    );
    v
}

/// Whether order management is degraded right now (a gap is open).
pub fn order_facts_failing(ctx: &AppState) -> bool {
    !ctx.reconciler.open_gaps().is_empty()
}

// ------------------------------------------------------------ read models

/// IST ISO-8601 timestamp (web `convert_to_ist(...).isoformat()`).
pub fn iso_ist(ts: &str) -> String {
    store::parse_ts(ts)
        .map(|t| {
            t.with_timezone(&chrono_tz::Asia::Kolkata)
                .format("%Y-%m-%dT%H:%M:%S%:z")
                .to_string()
        })
        .unwrap_or_else(|| ts.to_string())
}

pub fn current_json(m: &HealthRow) -> Value {
    json!({
        "timestamp": iso_ist(&m.timestamp),
        "fd": {
            "count": m.fd_count.unwrap_or(0),
            "limit": m.fd_limit,
            "usage_percent": m.fd_usage_percent.unwrap_or(0.0),
            "status": m.fd_status.clone().unwrap_or_else(|| "unknown".into()),
        },
        "memory": {
            "rss_mb": m.memory_rss_mb,
            "vms_mb": m.memory_vms_mb,
            "percent": m.memory_percent,
            "available_mb": m.memory_available_mb,
            "swap_mb": m.memory_swap_mb,
            "status": m.memory_status,
        },
        "database": {
            "total": m.db_connections_total,
            "connections": m.db_connections,
            "status": m.db_status,
            "sizes_mb": m.db_sizes,
        },
        "websocket": {
            "total": m.ws_connections_total,
            "connections": m.ws_connections,
            "total_symbols": m.ws_total_symbols,
            "status": m.ws_status,
        },
        "threads": {
            "count": m.thread_count,
            "stuck": m.stuck_threads,
            "status": m.thread_status,
            "details": m.thread_details,
        },
        "processes": m.process_details.clone().unwrap_or_else(|| json!([])),
        "overall_status": m.overall_status,
    })
}

pub fn history_json(rows: &[HealthRow]) -> Value {
    Value::Array(
        rows.iter()
            .map(|m| {
                json!({
                    "timestamp": iso_ist(&m.timestamp),
                    "fd_count": m.fd_count,
                    "memory_rss_mb": m.memory_rss_mb,
                    "db_connections": m.db_connections_total,
                    "ws_connections": m.ws_connections_total,
                    "threads": m.thread_count,
                    "overall_status": m.overall_status,
                })
            })
            .collect(),
    )
}

fn agg(v: &[f64]) -> (f64, f64, f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let cur = *v.last().unwrap_or(&0.0);
    let avg = v.iter().sum::<f64>() / v.len() as f64;
    let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    (cur, avg, min, max)
}

/// Web `HealthMetric.get_stats`.
pub fn stats_json(rows: &[HealthRow], hours: i64) -> Value {
    if rows.is_empty() {
        return json!({
            "total_samples": 0, "time_period_hours": hours, "fd": {}, "memory": {},
            "database": {}, "websocket": {}, "threads": {}, "status": {},
        });
    }
    let nums =
        |f: &dyn Fn(&HealthRow) -> Option<f64>| -> Vec<f64> { rows.iter().filter_map(f).collect() };
    let fd = agg(&nums(&|m| m.fd_count.map(|v| v as f64)));
    let mem = agg(&nums(&|m| m.memory_rss_mb));
    let db = agg(&nums(&|m| m.db_connections_total.map(|v| v as f64)));
    let ws = agg(&nums(&|m| m.ws_connections_total.map(|v| v as f64)));
    let th = agg(&nums(&|m| m.thread_count.map(|v| v as f64)));
    let cnt = |f: &dyn Fn(&HealthRow) -> Option<&String>, s: &str| {
        rows.iter()
            .filter(|m| f(m).map(|x| x == s).unwrap_or(false))
            .count()
    };
    let pair = |f: &dyn Fn(&HealthRow) -> Option<&String>| json!({"warn": cnt(f, "warn"), "fail": cnt(f, "fail")});
    let ow = cnt(&|m| m.overall_status.as_ref(), "warn");
    let of = cnt(&|m| m.overall_status.as_ref(), "fail");
    json!({
        "total_samples": rows.len(),
        "time_period_hours": hours,
        "fd": {"current": fd.0, "avg": fd.1, "min": fd.2, "max": fd.3,
               "fail_count": cnt(&|m| m.fd_status.as_ref(), "fail"),
               "warn_count": cnt(&|m| m.fd_status.as_ref(), "warn")},
        "memory": {"current_mb": mem.0, "avg_mb": mem.1, "min_mb": mem.2, "max_mb": mem.3,
                   "fail_count": cnt(&|m| m.memory_status.as_ref(), "fail"),
                   "warn_count": cnt(&|m| m.memory_status.as_ref(), "warn")},
        "database": {"current": db.0, "avg": db.1, "min": db.2, "max": db.3},
        "websocket": {"current": ws.0, "avg": ws.1, "min": ws.2, "max": ws.3},
        "threads": {"current": th.0, "avg": th.1, "min": th.2, "max": th.3},
        "status": {
            "overall": {"pass": rows.len() - ow - of, "warn": ow, "fail": of},
            "fd": pair(&|m| m.fd_status.as_ref()),
            "memory": pair(&|m| m.memory_status.as_ref()),
            "database": pair(&|m| m.db_status.as_ref()),
            "websocket": pair(&|m| m.ws_status.as_ref()),
            "threads": pair(&|m| m.thread_status.as_ref()),
        },
    })
}

pub fn alerts_json(rows: &[AlertRow]) -> Value {
    Value::Array(
        rows.iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "timestamp": iso_ist(&a.timestamp),
                    "alert_type": a.alert_type,
                    "severity": a.severity,
                    "metric_name": a.metric_name,
                    "metric_value": a.metric_value,
                    "threshold_value": a.threshold_value,
                    "message": a.message,
                    "acknowledged": a.acknowledged,
                    "resolved": a.resolved,
                })
            })
            .collect(),
    )
}

/// Clamp `hours` to the web's [1, 168].
pub fn clamp_hours(raw: Option<&str>) -> i64 {
    raw.and_then(|h| h.parse::<i64>().ok())
        .unwrap_or(24)
        .clamp(1, 168)
}

pub fn since(now: DateTime<Utc>, hours: i64) -> DateTime<Utc> {
    now - Duration::hours(hours)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_measurable() {
        let s = process_stats();
        assert!(s.rss_mb.unwrap_or(0.0) > 0.0);
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(s.fd_count.unwrap_or(0) > 0);
            assert!(s.threads.unwrap_or(0) > 0);
        }
    }

    #[test]
    fn statuses() {
        assert_eq!(status_of(Some(10.0), (5.0, 9.0)), "fail");
        assert_eq!(status_of(Some(6.0), (5.0, 9.0)), "warn");
        assert_eq!(status_of(None, (5.0, 9.0)), "unknown");
        assert_eq!(worst(&["pass", "warn", "unknown"]), "warn");
        assert_eq!(clamp_hours(Some("999")), 168);
        assert_eq!(clamp_hours(Some("x")), 24);
    }
}
