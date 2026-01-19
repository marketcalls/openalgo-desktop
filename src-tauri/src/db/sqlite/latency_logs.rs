//! Latency Logs Database Module
//!
//! Tracks API response times for performance monitoring.
//! Includes RTT, validation time, broker response time, and overhead.

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

/// Latency log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyLog {
    pub id: i64,
    pub timestamp: String,
    pub order_id: String,
    pub broker: String,
    pub symbol: String,
    pub order_type: String,         // MARKET, LIMIT, PLACE, SMART, etc.
    pub rtt_ms: f64,                // Round-trip time (comparable to Postman)
    pub validation_ms: f64,         // Pre-request processing
    pub broker_response_ms: f64,    // Broker API response time
    pub overhead_ms: f64,           // Our processing overhead
    pub total_ms: f64,              // Total latency
    pub status: String,             // SUCCESS, FAILED
    pub error: Option<String>,
}

/// Create latency log entry
pub fn log_latency(
    conn: &Connection,
    order_id: &str,
    broker: &str,
    symbol: &str,
    order_type: &str,
    rtt_ms: f64,
    validation_ms: f64,
    broker_response_ms: f64,
    overhead_ms: f64,
    total_ms: f64,
    status: &str,
    error: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO latency_logs (
            timestamp, order_id, broker, symbol, order_type,
            rtt_ms, validation_ms, broker_response_ms, overhead_ms, total_ms,
            status, error
        ) VALUES (datetime('now'), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            order_id, broker, symbol, order_type,
            rtt_ms, validation_ms, broker_response_ms, overhead_ms, total_ms,
            status, error
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get recent latency logs
pub fn get_recent_logs(conn: &Connection, limit: i64) -> Result<Vec<LatencyLog>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, order_id, broker, symbol, order_type,
                rtt_ms, validation_ms, broker_response_ms, overhead_ms, total_ms,
                status, error
         FROM latency_logs
         ORDER BY timestamp DESC
         LIMIT ?1"
    )?;

    let rows = stmt.query_map(params![limit], |row| {
        Ok(LatencyLog {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            order_id: row.get(2)?,
            broker: row.get(3)?,
            symbol: row.get(4)?,
            order_type: row.get(5)?,
            rtt_ms: row.get(6)?,
            validation_ms: row.get(7)?,
            broker_response_ms: row.get(8)?,
            overhead_ms: row.get(9)?,
            total_ms: row.get(10)?,
            status: row.get(11)?,
            error: row.get(12)?,
        })
    })?;

    rows.collect()
}

/// Latency statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyStats {
    pub total_orders: i64,
    pub failed_orders: i64,
    pub success_rate: f64,
    pub avg_rtt: f64,
    pub avg_overhead: f64,
    pub avg_total: f64,
    pub p50_total: f64,
    pub p90_total: f64,
    pub p95_total: f64,
    pub p99_total: f64,
    pub sla_100ms: f64,     // % under 100ms
    pub sla_150ms: f64,     // % under 150ms
    pub sla_200ms: f64,     // % under 200ms
    pub broker_stats: std::collections::HashMap<String, BrokerLatencyStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerLatencyStats {
    pub total_orders: i64,
    pub failed_orders: i64,
    pub avg_rtt: f64,
    pub avg_total: f64,
    pub p50_total: f64,
    pub p99_total: f64,
    pub sla_150ms: f64,
}

/// Get latency statistics
pub fn get_stats(conn: &Connection) -> Result<LatencyStats> {
    // Get overall stats
    let total_orders: i64 = conn.query_row(
        "SELECT COUNT(*) FROM latency_logs",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    let failed_orders: i64 = conn.query_row(
        "SELECT COUNT(*) FROM latency_logs WHERE status = 'FAILED'",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    let avg_rtt: f64 = conn.query_row(
        "SELECT COALESCE(AVG(rtt_ms), 0) FROM latency_logs",
        [],
        |row| row.get(0),
    ).unwrap_or(0.0);

    let avg_overhead: f64 = conn.query_row(
        "SELECT COALESCE(AVG(overhead_ms), 0) FROM latency_logs",
        [],
        |row| row.get(0),
    ).unwrap_or(0.0);

    let avg_total: f64 = conn.query_row(
        "SELECT COALESCE(AVG(total_ms), 0) FROM latency_logs",
        [],
        |row| row.get(0),
    ).unwrap_or(0.0);

    // SLA calculations
    let under_100: i64 = conn.query_row(
        "SELECT COUNT(*) FROM latency_logs WHERE total_ms < 100",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    let under_150: i64 = conn.query_row(
        "SELECT COUNT(*) FROM latency_logs WHERE total_ms < 150",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    let under_200: i64 = conn.query_row(
        "SELECT COUNT(*) FROM latency_logs WHERE total_ms < 200",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    let success_rate = if total_orders > 0 {
        ((total_orders - failed_orders) as f64 / total_orders as f64) * 100.0
    } else {
        0.0
    };

    let sla_100ms = if total_orders > 0 { (under_100 as f64 / total_orders as f64) * 100.0 } else { 0.0 };
    let sla_150ms = if total_orders > 0 { (under_150 as f64 / total_orders as f64) * 100.0 } else { 0.0 };
    let sla_200ms = if total_orders > 0 { (under_200 as f64 / total_orders as f64) * 100.0 } else { 0.0 };

    // Get percentiles (simplified - get all values and calculate)
    let mut all_latencies: Vec<f64> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT total_ms FROM latency_logs WHERE total_ms IS NOT NULL ORDER BY total_ms")?;
        let rows = stmt.query_map([], |row| row.get::<_, f64>(0))?;
        for row in rows {
            if let Ok(v) = row {
                all_latencies.push(v);
            }
        }
    }

    let (p50_total, p90_total, p95_total, p99_total) = calculate_percentiles(&all_latencies);

    // Get broker stats
    let mut broker_stats = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT broker, COUNT(*),
                    SUM(CASE WHEN status = 'FAILED' THEN 1 ELSE 0 END),
                    AVG(rtt_ms), AVG(total_ms),
                    SUM(CASE WHEN total_ms < 150 THEN 1 ELSE 0 END)
             FROM latency_logs
             WHERE broker IS NOT NULL
             GROUP BY broker"
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, f64>(3).unwrap_or(0.0),
                row.get::<_, f64>(4).unwrap_or(0.0),
                row.get::<_, i64>(5)?,
            ))
        })?;

        for row in rows {
            if let Ok((broker, total, failed, avg_rtt, avg_total, under_150)) = row {
                let sla = if total > 0 { (under_150 as f64 / total as f64) * 100.0 } else { 0.0 };

                // Get percentiles for this broker
                let mut broker_latencies: Vec<f64> = Vec::new();
                {
                    let mut pstmt = conn.prepare(
                        "SELECT total_ms FROM latency_logs WHERE broker = ?1 AND total_ms IS NOT NULL ORDER BY total_ms"
                    )?;
                    let prows = pstmt.query_map(params![&broker], |row| row.get::<_, f64>(0))?;
                    for prow in prows {
                        if let Ok(v) = prow {
                            broker_latencies.push(v);
                        }
                    }
                }
                let (p50, _, _, p99) = calculate_percentiles(&broker_latencies);

                broker_stats.insert(broker, BrokerLatencyStats {
                    total_orders: total,
                    failed_orders: failed,
                    avg_rtt,
                    avg_total,
                    p50_total: p50,
                    p99_total: p99,
                    sla_150ms: sla,
                });
            }
        }
    }

    Ok(LatencyStats {
        total_orders,
        failed_orders,
        success_rate,
        avg_rtt,
        avg_overhead,
        avg_total,
        p50_total,
        p90_total,
        p95_total,
        p99_total,
        sla_100ms,
        sla_150ms,
        sla_200ms,
        broker_stats,
    })
}

/// Calculate percentiles from sorted list
/// Uses the standard formula: index = percentile * (len - 1) for 0-indexed arrays
fn calculate_percentiles(sorted_values: &[f64]) -> (f64, f64, f64, f64) {
    if sorted_values.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }

    let len = sorted_values.len();
    if len == 1 {
        let val = sorted_values[0];
        return (val, val, val, val);
    }

    // Use (len - 1) for correct 0-indexed percentile calculation
    let max_idx = (len - 1) as f64;
    let p50_idx = (max_idx * 0.50) as usize;
    let p90_idx = (max_idx * 0.90) as usize;
    let p95_idx = (max_idx * 0.95) as usize;
    let p99_idx = (max_idx * 0.99) as usize;

    let p50 = sorted_values[p50_idx];
    let p90 = sorted_values[p90_idx];
    let p95 = sorted_values[p95_idx];
    let p99 = sorted_values[p99_idx];

    (p50, p90, p95, p99)
}

/// Purge old non-order latency logs (keep order logs forever)
pub fn purge_old_data_logs(conn: &Connection, days: i64) -> Result<usize> {
    // Order types to keep forever
    let order_types = vec!["PLACE", "SMART", "MODIFY", "CANCEL", "CLOSE", "CANCEL_ALL", "BASKET", "SPLIT", "OPTIONS", "OPTIONS_MULTI"];

    // Build parameterized query safely - days is passed as parameter, not interpolated
    let placeholders: Vec<String> = (2..=order_types.len() + 1).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "DELETE FROM latency_logs WHERE timestamp < datetime('now', ?1) AND order_type NOT IN ({})",
        placeholders.join(", ")
    );

    let days_modifier = format!("-{} days", days);
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&days_modifier];
    for ot in &order_types {
        params.push(ot);
    }

    let mut stmt = conn.prepare(&sql)?;
    let deleted = stmt.execute(params.as_slice())?;
    Ok(deleted)
}

/// Clear all latency logs
pub fn clear_all_logs(conn: &Connection) -> Result<usize> {
    let deleted = conn.execute("DELETE FROM latency_logs", [])?;
    Ok(deleted)
}
