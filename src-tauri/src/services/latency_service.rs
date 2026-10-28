//! `/api/v1` order latency recorded by the monitoring middleware
//! (`services::monitor`), with the web's statistics for the Latency page
//! (web `blueprints/latency.py`, `OrderLatency.get_latency_stats`):
//! numpy-style linear percentiles over the last 30 days and 30-bin
//! histograms. Every read writes out what is queued first.

use crate::db::sqlite::monitor::{self as store, LatencyRow};
use crate::error::Result;
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use futures_util::stream::{self, Stream};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Percentiles cover this many days (web `PERCENTILE_WINDOW_DAYS`).
const PERCENTILE_DAYS: i64 = 30;

/// numpy `percentile` (linear interpolation) on sorted data.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = p / 100.0 * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
}

/// Histogram bins (web `get_histogram_data`).
const BINS: usize = 30;

/// numpy `histogram(rtts, bins=30, range=(min, max))` of one broker's rtt,
/// counted by the store.
fn histogram(conn: &rusqlite::Connection, broker: &str, t: &store::LatencyTotals) -> Result<Value> {
    if t.total == 0 {
        return Ok(json!({"bins": [], "counts": [], "avg_rtt": 0, "min_rtt": 0, "max_rtt": 0}));
    }
    let (min, max) = (t.min_rtt, t.max_rtt);
    let (lo, hi) = if max > min {
        (min, max)
    } else {
        (min - 0.5, max + 0.5)
    };
    let width = (hi - lo) / BINS as f64;
    let counts = store::rtt_buckets(conn, broker, lo, width, BINS)?;
    let bins: Vec<String> = (0..BINS)
        .map(|i| format!("{:.1}", lo + width * i as f64))
        .collect();
    Ok(
        json!({"bins": bins, "counts": counts, "avg_rtt": t.avg_rtt, "min_rtt": min, "max_rtt": max}),
    )
}

/// The newest `limit` calls first, read off the async workers.
pub async fn recent(ctx: &Arc<AppState>, limit: i64) -> Result<Vec<LatencyRow>> {
    ctx.report(move |c| {
        c.monitor.drain_now(c);
        store::recent_latency(&*c.logs.conn()?, Some(limit))
    })
    .await
}

/// Rows per page of the export.
const EXPORT_PAGE: i64 = 1_000;

/// Every call, newest first, a page at a time (web `/latency/export` reads
/// them all at once), each page turned into text by `render`. A page is
/// read and rendered off the async workers when the download asks for it,
/// so at most one page is held (DB-03) and the runtime never formats one:
/// rendered there, a page of 1,000 rows held it 17 ms in a debug build,
/// 3.5 s over a 200,000-row export.
pub fn export_pages<F>(ctx: Arc<AppState>, render: F) -> impl Stream<Item = Result<String>> + Send
where
    F: Fn(&[LatencyRow]) -> String + Send + Sync + 'static,
{
    stream::try_unfold(Some((ctx, None, Arc::new(render))), |state| async move {
        let Some((ctx, before, render)) = state else {
            return Ok(None);
        };
        let r = render.clone();
        let (out, last) = ctx
            .report(move |c| {
                if before.is_none() {
                    c.monitor.drain_now(c);
                }
                let page = store::latency_page(&*c.logs.conn()?, before, EXPORT_PAGE)?;
                // A short page is the last one.
                let last = page
                    .last()
                    .filter(|_| page.len() as i64 == EXPORT_PAGE)
                    .map(|l| l.id);
                Ok((r(&page), last))
            })
            .await?;
        Ok(Some((out, last.map(|id| (ctx, Some(id), render)))))
    })
}

fn pct(n: i64, total: i64) -> f64 {
    if total > 0 {
        n as f64 / total as f64 * 100.0
    } else {
        0.0
    }
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// Web `OrderLatency.get_latency_stats` plus `broker_histograms`, read off
/// the async workers. A request that arrives while a report is computed
/// waits for it and answers with it (`ReportFlight`).
pub async fn stats(ctx: &Arc<AppState>) -> Result<Value> {
    ctx.latency_reports.share(ctx, stats_now).await
}

fn stats_now(ctx: &AppState) -> Result<Value> {
    ctx.monitor.drain_now(ctx);
    let c = ctx.logs.conn()?;
    report(&c, ctx.now())
}

/// Counts, averages and SLA shares over every row, aggregated in SQL;
/// percentiles over the 30 days before `now` (DB-03: nothing loads the
/// whole table).
fn report(c: &rusqlite::Connection, now: DateTime<Utc>) -> Result<Value> {
    let cutoff = store::ts(now - Duration::days(PERCENTILE_DAYS));
    let all = store::latency_totals(c)?;
    let recent = sorted(store::totals_since(c, &cutoff, None)?);
    let mut broker_stats = Map::new();
    let mut histograms = Map::new();
    for (b, t) in store::broker_latency_totals(c)? {
        let lat = sorted(store::totals_since(c, &cutoff, Some(&b))?);
        broker_stats.insert(
            b.clone(),
            json!({
                "total_orders": t.total,
                "failed_orders": t.failed,
                "avg_rtt": t.avg_rtt,
                "avg_overhead": t.avg_overhead,
                "avg_total": t.avg_total,
                "p50_total": percentile(&lat, 50.0),
                "p99_total": percentile(&lat, 99.0),
                "sla_150ms": pct(t.under_150, t.total),
            }),
        );
        histograms.insert(b.clone(), histogram(c, &b, &t)?);
    }
    Ok(json!({
        "total_orders": all.total,
        "failed_orders": all.failed,
        "success_rate": pct(all.total - all.failed, all.total),
        "avg_rtt": all.avg_rtt,
        "avg_overhead": all.avg_overhead,
        "avg_total": all.avg_total,
        "p50_total": percentile(&recent, 50.0),
        "p90_total": percentile(&recent, 90.0),
        "p95_total": percentile(&recent, 95.0),
        "p99_total": percentile(&recent, 99.0),
        "sla_100ms": pct(all.under_100, all.total),
        "sla_150ms": pct(all.under_150, all.total),
        "sla_200ms": pct(all.under_200, all.total),
        "broker_stats": broker_stats,
        "broker_histograms": histograms,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::logs::LogsDb;
    use std::collections::BTreeMap;

    #[test]
    fn numpy_percentiles() {
        let v = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&v, 50.0), 2.5);
        assert_eq!(percentile(&v, 100.0), 4.0);
    }

    /// (broker, rtt, overhead, total, failed, timestamp)
    type Point = (Option<String>, f64, f64, f64, bool, String);

    /// The report as it was computed before DB-03, from every row in
    /// memory: the golden values the SQL aggregates must keep. One change:
    /// `avg_overhead` is the rows' average (OBS-03), where the old code
    /// always said 0.
    fn reference(points: &[Point], cutoff: &str) -> Value {
        fn hist(values: &[f64]) -> Value {
            if values.is_empty() {
                return json!({"bins": [], "counts": [], "avg_rtt": 0, "min_rtt": 0, "max_rtt": 0});
            }
            let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let avg = values.iter().sum::<f64>() / values.len() as f64;
            let (lo, hi) = if max > min {
                (min, max)
            } else {
                (min - 0.5, max + 0.5)
            };
            let width = (hi - lo) / 30.0;
            let mut counts = vec![0i64; 30];
            for v in values {
                let i = (((v - lo) / width).floor() as usize).min(29);
                counts[i] += 1;
            }
            let bins: Vec<String> = (0..30)
                .map(|i| format!("{:.1}", lo + width * i as f64))
                .collect();
            json!({"bins": bins, "counts": counts, "avg_rtt": avg, "min_rtt": min, "max_rtt": max})
        }
        let avg = |v: Vec<f64>| {
            if v.is_empty() {
                0.0
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        };
        let recent_sorted = |ps: &[&Point]| {
            sorted(
                ps.iter()
                    .filter(|p| p.5.as_str() >= cutoff)
                    .map(|p| p.3)
                    .collect(),
            )
        };
        let all: Vec<&Point> = points.iter().collect();
        let n = all.len() as i64;
        let failed = all.iter().filter(|p| p.4).count() as i64;
        let under = |ms: f64| all.iter().filter(|p| p.3 < ms).count() as i64;
        let recent = recent_sorted(&all);
        let mut by_broker: BTreeMap<String, Vec<&Point>> = BTreeMap::new();
        for p in points {
            if let Some(b) = &p.0 {
                by_broker.entry(b.clone()).or_default().push(p);
            }
        }
        let mut bs = Map::new();
        let mut hs = Map::new();
        for (b, ps) in &by_broker {
            let lat = recent_sorted(ps);
            let bn = ps.len() as i64;
            bs.insert(
                b.clone(),
                json!({
                    "total_orders": bn,
                    "failed_orders": ps.iter().filter(|p| p.4).count(),
                    "avg_rtt": avg(ps.iter().map(|p| p.1).collect()),
                    "avg_overhead": avg(ps.iter().map(|p| p.2).collect()),
                    "avg_total": avg(ps.iter().map(|p| p.3).collect()),
                    "p50_total": percentile(&lat, 50.0),
                    "p99_total": percentile(&lat, 99.0),
                    "sla_150ms": pct(ps.iter().filter(|p| p.3 < 150.0).count() as i64, bn),
                }),
            );
            hs.insert(b.clone(), hist(&ps.iter().map(|p| p.1).collect::<Vec<_>>()));
        }
        json!({
            "total_orders": n,
            "failed_orders": failed,
            "success_rate": pct(n - failed, n),
            "avg_rtt": avg(all.iter().map(|p| p.1).collect()),
            "avg_overhead": avg(all.iter().map(|p| p.2).collect()),
            "avg_total": avg(all.iter().map(|p| p.3).collect()),
            "p50_total": percentile(&recent, 50.0),
            "p90_total": percentile(&recent, 90.0),
            "p95_total": percentile(&recent, 95.0),
            "p99_total": percentile(&recent, 99.0),
            "sla_100ms": pct(under(100.0), n),
            "sla_150ms": pct(under(150.0), n),
            "sla_200ms": pct(under(200.0), n),
            "broker_stats": bs,
            "broker_histograms": hs,
        })
    }

    /// Equal, numbers within a relative 1e-9 (SQLite sums in its own order).
    fn same(a: &Value, b: &Value, at: &str) {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => {
                let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
                assert!(
                    (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0),
                    "{}: {} != {}",
                    at,
                    x,
                    y
                );
            }
            (Value::Object(x), Value::Object(y)) => {
                assert_eq!(
                    x.keys().collect::<Vec<_>>(),
                    y.keys().collect::<Vec<_>>(),
                    "{}",
                    at
                );
                for (k, v) in x {
                    same(v, &y[k], &format!("{}.{}", at, k));
                }
            }
            (Value::Array(x), Value::Array(y)) => {
                assert_eq!(x.len(), y.len(), "{}", at);
                for (i, (v, w)) in x.iter().zip(y).enumerate() {
                    same(v, w, &format!("{}[{}]", at, i));
                }
            }
            _ => assert_eq!(a, b, "{}", at),
        }
    }

    /// DB-03: the SQL report keeps the values the in-memory one gave, with
    /// rows inside and outside the 30-day percentile window, failures, rows
    /// without a broker, and a broker whose calls all took the same time.
    #[test]
    fn the_sql_report_matches_the_in_memory_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = LogsDb::new(&dir.path().join("logs.db")).unwrap();
        let conn = db.conn().unwrap();
        let now = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 10, 5, 4, 30, 0).unwrap();
        let mut seed: u64 = 7;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as f64 / (1u64 << 31) as f64
        };
        let mut points: Vec<Point> = Vec::new();
        for i in 0..600 {
            let broker = match i % 4 {
                0 => Some("zerodha"),
                1 => Some("dhan"),
                2 => None,
                _ => Some("fyers"),
            };
            let rtt = if broker == Some("fyers") {
                50.0
            } else {
                (next() * 400.0 * 100.0).round() / 100.0
            };
            let overhead = (next() * 30.0 * 100.0).round() / 100.0;
            let at = store::ts(now - Duration::minutes((next() * 60.0 * 24.0 * 60.0) as i64));
            let failed = i % 7 == 0;
            store::insert_latency(
                &conn,
                &LatencyRow {
                    timestamp: at.clone(),
                    order_id: i.to_string(),
                    broker: broker.map(String::from),
                    order_type: "PLACE".into(),
                    rtt_ms: rtt,
                    overhead_ms: overhead,
                    total_latency_ms: rtt + overhead,
                    status: if failed { "FAILED" } else { "SUCCESS" }.into(),
                    ..Default::default()
                },
            )
            .unwrap();
            points.push((
                broker.map(String::from),
                rtt,
                overhead,
                rtt + overhead,
                failed,
                at,
            ));
        }
        let cutoff = store::ts(now - Duration::days(PERCENTILE_DAYS));
        let want = reference(&points, &cutoff);
        let got = report(&conn, now).unwrap();
        same(&got, &want, "stats");
        assert_eq!(got["broker_histograms"]["fyers"]["counts"][15], 150);
    }

    #[test]
    fn an_empty_log_reports_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let db = LogsDb::new(&dir.path().join("logs.db")).unwrap();
        let got = report(&db.conn().unwrap(), Utc::now()).unwrap();
        same(&got, &reference(&[], "0"), "stats");
    }
}
