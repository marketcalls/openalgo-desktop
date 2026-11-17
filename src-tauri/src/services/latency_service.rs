//! `/api/v1` call latency recorded by the monitoring middleware
//! (`services::monitor`), reported one group of endpoints at a time for the
//! Latency page (web `blueprints/latency.py`, `OrderLatency.get_latency_stats`,
//! web c46a4b19a). Which endpoint is in which group is
//! `db::sqlite::monitor::latency_groups`. Counts, averages and the speed
//! bands cover every record of the group, in SQL; numpy-style linear
//! percentiles cover the last 30 days. Every read writes out what is queued
//! first, and runs off the async workers.

use crate::db::sqlite::monitor::{
    self as store, latency_groups as groups, LatencyFilter, LatencyRow, LatencySums,
};
use crate::error::Result;
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use futures_util::stream::{self, Stream};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
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

/// A broker's figures in a group, with the histogram of every record of
/// that broker (web `get_broker_stats`), read off the async workers;
/// `None` when the broker has no record in the group (the web's 404).
pub async fn broker_page(
    ctx: &Arc<AppState>,
    group: &'static str,
    broker: String,
) -> Result<Option<Map<String, Value>>> {
    let figures = stats(ctx, group).await?;
    let Some(Value::Object(mut row)) = figures["broker_stats"].get(&broker).cloned() else {
        return Ok(None);
    };
    let histogram = ctx
        .report(move |c| broker_histogram(&*c.logs.conn()?, &broker))
        .await?;
    row.insert("histogram".into(), histogram);
    Ok(Some(row))
}

/// numpy `histogram(rtts, bins=30, range=(min, max))` of every record of
/// one broker (web `get_histogram_data(broker)`): numpy widens a range of
/// one value to (value - 0.5, value + 0.5).
fn broker_histogram(conn: &rusqlite::Connection, broker: &str) -> Result<Value> {
    let (count, avg, min, max) = store::broker_rtt(conn, broker)?;
    if count == 0 {
        return Ok(json!({"bins": [], "counts": [], "avg_rtt": 0, "min_rtt": 0, "max_rtt": 0}));
    }
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
    Ok(json!({"bins": bins, "counts": counts, "avg_rtt": avg, "min_rtt": min, "max_rtt": max}))
}

/// The newest `limit` calls of a group, newest first, read off the async
/// workers: the table headed Recent Orders lists orders, never pushed out
/// by a chart loading its history.
pub async fn recent(
    ctx: &Arc<AppState>,
    group: &'static str,
    limit: i64,
) -> Result<Vec<LatencyRow>> {
    ctx.report(move |c| {
        c.monitor.drain_now(c);
        let filter = LatencyFilter::for_group(group);
        store::recent_latency(&*c.logs.conn()?, &filter, Some(limit))
    })
    .await
}

/// Rows per page of the export.
const EXPORT_PAGE: i64 = 1_000;

/// Every call of a group, newest first, a page at a time (web
/// `/latency/export` reads them all at once), each page turned into text by
/// `render`. A page is read and rendered off the async workers when the
/// download asks for it, so at most one page is held (DB-03) and the
/// runtime never formats one: rendered there, a page of 1,000 rows held it
/// 17 ms in a debug build, 3.5 s over a 200,000-row export.
pub fn export_pages<F>(
    ctx: Arc<AppState>,
    group: &'static str,
    render: F,
) -> impl Stream<Item = Result<String>> + Send
where
    F: Fn(&[LatencyRow]) -> String + Send + Sync + 'static,
{
    stream::try_unfold(
        Some((ctx, None, Arc::new(render))),
        move |state| async move {
            let Some((ctx, before, render)) = state else {
                return Ok(None);
            };
            let r = render.clone();
            let (out, last) = ctx
                .report(move |c| {
                    if before.is_none() {
                        c.monitor.drain_now(c);
                    }
                    let filter = LatencyFilter::for_group(group);
                    let page = store::latency_page(&*c.logs.conn()?, &filter, before, EXPORT_PAGE)?;
                    // A short page is the last one.
                    let last = page
                        .last()
                        .filter(|_| page.len() as i64 == EXPORT_PAGE)
                        .map(|l| l.id);
                    Ok((r(&page), last))
                })
                .await?;
            Ok(Some((out, last.map(|id| (ctx, Some(id), render)))))
        },
    )
}

/// A group's figures and the list of groups with their counts, read off
/// the async workers. A request for a group that arrives while its report
/// is computed waits for it and answers with it (`KeyedFlight`); each
/// group has its own.
pub async fn stats(ctx: &Arc<AppState>, group: &'static str) -> Result<Value> {
    ctx.latency_reports
        .share(group, || {
            ctx.report(move |c| {
                c.monitor.drain_now(c);
                report(&*c.logs.conn()?, group, c.now())
            })
        })
        .await
}

fn share(part: i64, of: i64) -> f64 {
    if of > 0 {
        part as f64 / of as f64 * 100.0
    } else {
        0.0
    }
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// The figures every row shares (web `_Totals.figures`).
fn figures(t: &LatencySums) -> Map<String, Value> {
    let avg = |sum: f64| {
        if t.count > 0 {
            sum / t.count as f64
        } else {
            0.0
        }
    };
    let mut m = Map::new();
    m.insert("total_orders".into(), json!(t.count));
    m.insert("failed_orders".into(), json!(t.failed));
    m.insert(
        "success_rate".into(),
        json!(share(t.count - t.failed, t.count)),
    );
    m.insert("avg_rtt".into(), json!(avg(t.rtt)));
    m.insert("avg_overhead".into(), json!(avg(t.overhead)));
    m.insert("avg_total".into(), json!(avg(t.total)));
    m
}

/// p50 and p99 of a set of values, on a row.
fn with_percentiles(mut m: Map<String, Value>, values: Option<&Vec<f64>>) -> Map<String, Value> {
    let v = values.cloned().map(sorted).unwrap_or_default();
    m.insert("p50_total".into(), json!(percentile(&v, 50.0)));
    m.insert("p99_total".into(), json!(percentile(&v, 99.0)));
    m
}

/// Sums per tag, rows under a legacy spelling added to the tag they stand
/// for (web `_totals_by_tag`).
fn sums_by_canonical_tag(c: &rusqlite::Connection) -> Result<Vec<(Option<String>, LatencySums)>> {
    let mut out: Vec<(Option<String>, LatencySums)> = Vec::new();
    for (tag, sums) in store::sums_by_tag(c)? {
        let tag = tag.map(|t| groups::canonical(&t).to_string());
        match out.iter_mut().find(|(t, _)| *t == tag) {
            Some((_, s)) => s.add(&sums),
            None => out.push((tag, sums)),
        }
    }
    Ok(out)
}

/// A group's figures (web `get_latency_stats`): see the module doc.
fn report(c: &rusqlite::Connection, group: &'static str, now: DateTime<Utc>) -> Result<Value> {
    let by_tag = sums_by_canonical_tag(c)?;
    let mut stats = empty_stats(group, &by_tag);
    let selected: Vec<&(Option<String>, LatencySums)> = by_tag
        .iter()
        .filter(|(t, _)| group == groups::ALL || groups::group_key_of(t.as_deref()) == group)
        .collect();
    if selected.is_empty() {
        return Ok(Value::Object(stats));
    }
    let filter = LatencyFilter::for_group(group);
    let cutoff = store::ts(now - Duration::days(PERCENTILE_DAYS));
    let mut every = Vec::new();
    let mut per_tag: HashMap<Option<String>, Vec<f64>> = HashMap::new();
    let mut per_broker: HashMap<String, Vec<f64>> = HashMap::new();
    for r in store::recent_totals(c, &cutoff, &filter)? {
        every.push(r.total_ms);
        let tag = r.tag.map(|t| groups::canonical(&t).to_string());
        per_tag.entry(tag).or_default().push(r.total_ms);
        if let Some(b) = r.broker {
            per_broker.entry(b).or_default().push(r.total_ms);
        }
    }
    let every = sorted(every);
    let mut total = LatencySums::default();
    selected.iter().for_each(|(_, s)| total.add(s));
    stats.extend(figures(&total));
    for (key, p) in [
        ("p50_total", 50.0),
        ("p90_total", 90.0),
        ("p95_total", 95.0),
        ("p99_total", 99.0),
    ] {
        stats.insert(key.into(), json!(percentile(&every, p)));
    }
    stats.insert(
        "sla_100ms".into(),
        json!(share(total.under_100, total.count)),
    );
    stats.insert(
        "sla_150ms".into(),
        json!(share(total.under_150, total.count)),
    );
    stats.insert(
        "sla_200ms".into(),
        json!(share(total.under_200, total.count)),
    );
    stats.insert(
        "distribution".into(),
        json!({
            "excellent": total.under_150,
            "good": total.under_250 - total.under_150,
            "acceptable": total.under_400 - total.under_250,
            "slow": total.count - total.under_400,
        }),
    );
    stats.insert("endpoints".into(), endpoints(group, &selected, &per_tag));
    let mut brokers = Map::new();
    for (b, sums) in store::broker_sums(c, &filter)? {
        let mut row = with_percentiles(figures(&sums), per_broker.get(&b));
        row.insert("sla_150ms".into(), json!(share(sums.under_150, sums.count)));
        brokers.insert(b, Value::Object(row));
    }
    stats.insert("broker_stats".into(), Value::Object(brokers));
    Ok(Value::Object(stats))
}

/// One row per endpoint of the group: the group's own order, then any
/// other tag by name, no tag last.
fn endpoints(
    group: &str,
    selected: &[&(Option<String>, LatencySums)],
    per_tag: &HashMap<Option<String>, Vec<f64>>,
) -> Value {
    let order = groups::endpoint_order(group);
    let mut rows: Vec<_> = selected.to_vec();
    rows.sort_by_key(|(t, _)| {
        let at = t
            .as_deref()
            .and_then(|t| order.iter().position(|o| *o == t))
            .unwrap_or(order.len());
        (at, t.is_none(), t.clone().unwrap_or_default())
    });
    let rows = rows.into_iter().map(|(tag, sums)| {
        let mut row = with_percentiles(figures(sums), per_tag.get(tag));
        row.insert("type".into(), json!(tag));
        row.insert("label".into(), json!(groups::label_of(tag.as_deref())));
        Value::Object(row)
    });
    Value::Array(rows.collect())
}

/// The figures of a group with no records, and the list of groups with
/// their counts (web `_empty_stats`): the same keys whatever was found.
fn empty_stats(
    group: &'static str,
    by_tag: &[(Option<String>, LatencySums)],
) -> Map<String, Value> {
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for (tag, sums) in by_tag {
        *counts
            .entry(groups::group_key_of(tag.as_deref()))
            .or_default() += sums.count;
    }
    let everything: i64 = counts.values().sum();
    counts.insert(groups::ALL, everything);
    let mut listed: Vec<&groups::Group> = groups::GROUPS.iter().collect();
    if counts.get(groups::OTHER).is_some_and(|n| *n > 0) || group == groups::OTHER {
        listed.push(&groups::OTHER_GROUP);
    }
    listed.push(&groups::ALL_GROUP);
    let listed: Vec<Value> = listed
        .iter()
        .map(|g| {
            json!({"key": g.key, "label": g.label, "description": g.description,
                   "count": counts.get(g.key).copied().unwrap_or(0), "rated": g.rated})
        })
        .collect();
    let chosen = groups::get_group(group).unwrap_or(&groups::ALL_GROUP);
    let zero = 0.0;
    let v = json!({
        "group": group,
        "groups": listed,
        "label": chosen.label,
        "description": chosen.description,
        "rated": chosen.rated,
        // None when the records are kept.
        "retention_days": (!chosen.kept).then_some(groups::RETENTION_DAYS),
        "percentile_window_days": PERCENTILE_DAYS,
        "total_orders": 0, "failed_orders": 0, "success_rate": zero,
        "avg_rtt": zero, "avg_overhead": zero, "avg_total": zero,
        "p50_total": zero, "p90_total": zero, "p95_total": zero, "p99_total": zero,
        "sla_100ms": zero, "sla_150ms": zero, "sla_200ms": zero,
        "distribution": {"excellent": 0, "good": 0, "acceptable": 0, "slow": 0},
        "endpoints": [],
        "broker_stats": {},
    });
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests;
