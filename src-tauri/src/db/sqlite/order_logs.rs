//! Order logs for audit trail
//!
//! Provides a complete audit trail of all order actions (place, modify, cancel)
//! for compliance and debugging purposes.

use crate::error::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Order log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderLog {
    pub id: i64,
    pub order_id: Option<String>,
    pub broker: String,
    pub symbol: String,
    pub exchange: String,
    pub side: String,
    pub quantity: i32,
    pub price: Option<f64>,
    pub order_type: String,
    pub product: String,
    pub status: String,
    pub message: Option<String>,
    pub source: Option<String>,
    pub created_at: String,
}

/// Create a new order log entry
pub fn create_log(
    conn: &Connection,
    order_id: Option<&str>,
    broker: &str,
    symbol: &str,
    exchange: &str,
    side: &str,
    quantity: i32,
    price: Option<f64>,
    order_type: &str,
    product: &str,
    status: &str,
    message: Option<&str>,
    source: Option<&str>,
) -> Result<i64> {
    conn.execute(
        r#"
        INSERT INTO order_logs (order_id, broker, symbol, exchange, side, quantity, price, order_type, product, status, message, source)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        "#,
        params![order_id, broker, symbol, exchange, side, quantity, price, order_type, product, status, message, source],
    )?;

    let id = conn.last_insert_rowid();
    tracing::debug!("Created order log entry: id={}, order_id={:?}, status={}", id, order_id, status);

    Ok(id)
}

/// Get order logs with optional filters
pub fn get_logs(
    conn: &Connection,
    limit: usize,
    offset: usize,
    broker: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<OrderLog>> {
    let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = {
        let mut conditions = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(b) = broker {
            conditions.push("broker = ?");
            params.push(Box::new(b.to_string()));
        }

        if let Some(s) = status {
            conditions.push("status = ?");
            params.push(Box::new(s.to_string()));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        params.push(Box::new(limit as i64));
        params.push(Box::new(offset as i64));

        (
            format!(
                r#"
                SELECT id, order_id, broker, symbol, exchange, side, quantity, price,
                       order_type, product, status, message, source, created_at
                FROM order_logs
                {}
                ORDER BY created_at DESC
                LIMIT ? OFFSET ?
                "#,
                where_clause
            ),
            params,
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

    let logs = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok(OrderLog {
                id: row.get(0)?,
                order_id: row.get(1)?,
                broker: row.get(2)?,
                symbol: row.get(3)?,
                exchange: row.get(4)?,
                side: row.get(5)?,
                quantity: row.get(6)?,
                price: row.get(7)?,
                order_type: row.get(8)?,
                product: row.get(9)?,
                status: row.get(10)?,
                message: row.get(11)?,
                source: row.get(12)?,
                created_at: row.get(13)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(logs)
}

/// Get logs for a specific order
pub fn get_logs_by_order_id(conn: &Connection, order_id: &str) -> Result<Vec<OrderLog>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, order_id, broker, symbol, exchange, side, quantity, price,
               order_type, product, status, message, source, created_at
        FROM order_logs
        WHERE order_id = ?1
        ORDER BY created_at ASC
        "#,
    )?;

    let logs = stmt
        .query_map(params![order_id], |row| {
            Ok(OrderLog {
                id: row.get(0)?,
                order_id: row.get(1)?,
                broker: row.get(2)?,
                symbol: row.get(3)?,
                exchange: row.get(4)?,
                side: row.get(5)?,
                quantity: row.get(6)?,
                price: row.get(7)?,
                order_type: row.get(8)?,
                product: row.get(9)?,
                status: row.get(10)?,
                message: row.get(11)?,
                source: row.get(12)?,
                created_at: row.get(13)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(logs)
}

/// Get recent logs (for dashboard)
pub fn get_recent_logs(conn: &Connection, limit: usize) -> Result<Vec<OrderLog>> {
    get_logs(conn, limit, 0, None, None)
}

/// Count total logs (with optional filters)
pub fn count_logs(conn: &Connection, broker: Option<&str>, status: Option<&str>) -> Result<i64> {
    let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = {
        let mut conditions = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(b) = broker {
            conditions.push("broker = ?");
            params.push(Box::new(b.to_string()));
        }

        if let Some(s) = status {
            conditions.push("status = ?");
            params.push(Box::new(s.to_string()));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        (
            format!("SELECT COUNT(*) FROM order_logs {}", where_clause),
            params,
        )
    };

    let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
    let count: i64 = conn.query_row(&sql, params_refs.as_slice(), |row| row.get(0))?;

    Ok(count)
}

/// Clear old logs (older than specified days)
pub fn clear_old_logs(conn: &Connection, days: i32) -> Result<usize> {
    let rows = conn.execute(
        "DELETE FROM order_logs WHERE created_at < datetime('now', ?1)",
        params![format!("-{} days", days)],
    )?;

    if rows > 0 {
        tracing::info!("Cleared {} old order log entries (older than {} days)", rows, days);
    }

    Ok(rows)
}

/// Get log statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogStats {
    pub total: i64,
    pub success: i64,
    pub failed: i64,
    pub pending: i64,
}

pub fn get_stats(conn: &Connection) -> Result<LogStats> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM order_logs", [], |row| row.get(0))?;
    let success: i64 = conn.query_row(
        "SELECT COUNT(*) FROM order_logs WHERE status IN ('success', 'complete', 'filled')",
        [],
        |row| row.get(0),
    )?;
    let failed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM order_logs WHERE status IN ('failed', 'rejected', 'error')",
        [],
        |row| row.get(0),
    )?;
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM order_logs WHERE status IN ('pending', 'submitted', 'open')",
        [],
        |row| row.get(0),
    )?;

    Ok(LogStats {
        total,
        success,
        failed,
        pending,
    })
}
