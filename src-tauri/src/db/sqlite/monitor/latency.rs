//! `/api/v1` order latency (web `latency_db.OrderLatency`).

use super::cap_text;
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

pub fn recent_latency(conn: &Connection, limit: Option<i64>) -> Result<Vec<LatencyRow>> {
    let mut st = conn.prepare(
        "SELECT id, timestamp, order_id, broker, symbol, order_type, rtt_ms, validation_latency_ms,
                response_latency_ms, overhead_ms, total_latency_ms, status, error
         FROM order_latency ORDER BY id DESC LIMIT ?1",
    )?;
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

/// Aggregates over a set of latency rows (web `get_latency_stats`: counts,
/// averages and SLA counts in SQL, over every row kept).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LatencyTotals {
    pub total: i64,
    pub failed: i64,
    pub avg_rtt: f64,
    pub avg_overhead: f64,
    pub avg_total: f64,
    pub under_100: i64,
    pub under_150: i64,
    pub under_200: i64,
    /// The rtt range, for the histogram.
    pub min_rtt: f64,
    pub max_rtt: f64,
}

const TOTALS: &str = "COUNT(*), COALESCE(SUM(status = 'FAILED'), 0),
    COALESCE(AVG(COALESCE(rtt_ms, 0)), 0), COALESCE(AVG(COALESCE(overhead_ms, 0)), 0),
    COALESCE(AVG(total_latency_ms), 0), COALESCE(SUM(total_latency_ms < 100), 0),
    COALESCE(SUM(total_latency_ms < 150), 0), COALESCE(SUM(total_latency_ms < 200), 0),
    COALESCE(MIN(COALESCE(rtt_ms, 0)), 0), COALESCE(MAX(COALESCE(rtt_ms, 0)), 0)";

fn totals_at(r: &rusqlite::Row, at: usize) -> rusqlite::Result<LatencyTotals> {
    Ok(LatencyTotals {
        total: r.get(at)?,
        failed: r.get(at + 1)?,
        avg_rtt: r.get(at + 2)?,
        avg_overhead: r.get(at + 3)?,
        avg_total: r.get(at + 4)?,
        under_100: r.get(at + 5)?,
        under_150: r.get(at + 6)?,
        under_200: r.get(at + 7)?,
        min_rtt: r.get(at + 8)?,
        max_rtt: r.get(at + 9)?,
    })
}

/// Totals over every row.
pub fn latency_totals(conn: &Connection) -> Result<LatencyTotals> {
    let sql = format!("SELECT {} FROM order_latency", TOTALS);
    Ok(conn.query_row(&sql, [], |r| totals_at(r, 0))?)
}

/// Totals per broker, for the rows that name one, by broker name.
pub fn broker_latency_totals(conn: &Connection) -> Result<Vec<(String, LatencyTotals)>> {
    let sql = format!(
        "SELECT broker, {} FROM order_latency WHERE broker IS NOT NULL GROUP BY broker ORDER BY broker",
        TOTALS
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, totals_at(r, 1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `total_latency_ms` of the rows since `since` (a stored timestamp), of one
/// broker or of all, for percentiles.
pub fn totals_since(conn: &Connection, since: &str, broker: Option<&str>) -> Result<Vec<f64>> {
    let mut st = conn.prepare(
        "SELECT total_latency_ms FROM order_latency
         WHERE timestamp >= ?1 AND (?2 IS NULL OR broker = ?2) AND total_latency_ms IS NOT NULL",
    )?;
    let rows = st
        .query_map(params![since, broker], |r| r.get(0))?
        .collect::<std::result::Result<Vec<f64>, _>>()?;
    Ok(rows)
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

/// One page of rows, newest first, older than `before_id` (all when
/// `None`), for the streamed export.
pub fn latency_page(
    conn: &Connection,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<LatencyRow>> {
    let mut st = conn.prepare(
        "SELECT id, timestamp, order_id, broker, symbol, order_type, rtt_ms, validation_latency_ms,
                response_latency_ms, overhead_ms, total_latency_ms, status, error
         FROM order_latency WHERE ?1 IS NULL OR id < ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = st
        .query_map(params![before_id, limit], latency_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
