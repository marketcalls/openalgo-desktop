//! `/api/v1` order latency (web `latency_db.OrderLatency`).

use super::cap_text;
use super::LatencyFilter;
use crate::error::Result;
use rusqlite::{params, Connection};

#[derive(Debug, Clone, Default)]
pub struct LatencyRow {
    pub id: i64,
    pub timestamp: String,
    pub order_id: String,
    pub broker: Option<String>,
    pub symbol: Option<String>,
    pub order_type: String,
    pub rtt_ms: f64,
    pub validation_latency_ms: f64,
    pub response_latency_ms: f64,
    pub overhead_ms: f64,
    pub total_latency_ms: f64,
    pub status: String,
    pub error: Option<String>,
}

const COLUMNS: &str = "id, timestamp, order_id, broker, symbol, order_type, rtt_ms,
    validation_latency_ms, response_latency_ms, overhead_ms, total_latency_ms, status, error";

pub fn insert_latency(conn: &Connection, l: &LatencyRow) -> Result<()> {
    conn.execute(
        "INSERT INTO order_latency (timestamp, order_id, broker, symbol, order_type, rtt_ms,
            validation_latency_ms, response_latency_ms, overhead_ms, total_latency_ms, status, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            l.timestamp,
            cap_text(&l.order_id, 100),
            l.broker.as_deref().map(|s| cap_text(s, 50)),
            l.symbol.as_deref().map(|s| cap_text(s, 50)),
            cap_text(&l.order_type, 20),
            l.rtt_ms,
            l.validation_latency_ms,
            l.response_latency_ms,
            l.overhead_ms,
            l.total_latency_ms,
            l.status,
            l.error.as_deref().map(|s| cap_text(s, 500)),
        ],
    )?;
    Ok(())
}

/// The newest `limit` records `filter` picks (all when `None`), newest
/// first: a group's recent list is not pushed out by other calls.
pub fn recent_latency(
    conn: &Connection,
    filter: &LatencyFilter,
    limit: Option<i64>,
) -> Result<Vec<LatencyRow>> {
    let mut st = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM order_latency WHERE {} ORDER BY id DESC LIMIT ?1",
        filter.sql()
    ))?;
    let rows = st
        .query_map([limit.unwrap_or(-1)], latency_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn latency_row(r: &rusqlite::Row) -> rusqlite::Result<LatencyRow> {
    Ok(LatencyRow {
        id: r.get(0)?,
        timestamp: r.get(1)?,
        order_id: r.get(2)?,
        broker: r.get(3)?,
        symbol: r.get(4)?,
        order_type: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        rtt_ms: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
        validation_latency_ms: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
        response_latency_ms: r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
        overhead_ms: r.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
        total_latency_ms: r.get(10)?,
        status: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
        error: r.get(12)?,
    })
}

/// How many of a broker's rows fall in each of `bins` rtt buckets of
/// `width` from `lo`: bucket `floor((rtt - lo) / width)`, the last one
/// closed (numpy `histogram`).
pub fn rtt_buckets(
    conn: &Connection,
    broker: &str,
    lo: f64,
    width: f64,
    bins: usize,
) -> Result<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT MIN(CAST((COALESCE(rtt_ms, 0) - ?2) / ?3 AS INTEGER), ?4 - 1) AS bucket, COUNT(*)
         FROM order_latency WHERE broker = ?1 GROUP BY bucket",
    )?;
    let mut counts = vec![0i64; bins];
    let rows = st.query_map(params![broker, lo, width, bins as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (bucket, n) = row?;
        if let Some(c) = usize::try_from(bucket).ok().and_then(|b| counts.get_mut(b)) {
            *c += n;
        }
    }
    Ok(counts)
}

/// One page of the rows `filter` picks, newest first, older than
/// `before_id` (all when `None`), for the streamed export.
pub fn latency_page(
    conn: &Connection,
    filter: &LatencyFilter,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<LatencyRow>> {
    let mut st = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM order_latency WHERE (?1 IS NULL OR id < ?1) AND {}
         ORDER BY id DESC LIMIT ?2",
        filter.sql()
    ))?;
    let rows = st
        .query_map(params![before_id, limit], latency_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
