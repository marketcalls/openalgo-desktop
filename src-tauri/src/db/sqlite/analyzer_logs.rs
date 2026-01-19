//! Analyzer Logs Database Module
//!
//! Handles logging for paper trading (analyze mode) operations.
//! Separate from live order_logs for clear distinction.

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

/// Analyzer log entry for paper trading
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerLog {
    pub id: i64,
    pub api_type: String,           // placeorder, cancelorder, modifyorder, etc.
    pub request_data: String,       // JSON request
    pub response_data: String,      // JSON response
    pub created_at: String,
}

/// Create analyzer log entry
pub fn create_log(
    conn: &Connection,
    api_type: &str,
    request_data: &str,
    response_data: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO analyzer_logs (api_type, request_data, response_data, created_at)
         VALUES (?1, ?2, ?3, datetime('now'))",
        params![api_type, request_data, response_data],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get analyzer logs with pagination
pub fn get_logs(
    conn: &Connection,
    api_type: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<AnalyzerLog>> {
    let mut logs = Vec::new();

    if let Some(api_t) = api_type {
        let mut stmt = conn.prepare(
            "SELECT id, api_type, request_data, response_data, created_at
             FROM analyzer_logs
             WHERE api_type = ?1
             ORDER BY created_at DESC LIMIT ?2 OFFSET ?3"
        )?;

        let rows = stmt.query_map(params![api_t, limit, offset], |row| {
            Ok(AnalyzerLog {
                id: row.get(0)?,
                api_type: row.get(1)?,
                request_data: row.get(2)?,
                response_data: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;

        for row in rows {
            logs.push(row?);
        }
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, api_type, request_data, response_data, created_at
             FROM analyzer_logs
             ORDER BY created_at DESC LIMIT ?1 OFFSET ?2"
        )?;

        let rows = stmt.query_map(params![limit, offset], |row| {
            Ok(AnalyzerLog {
                id: row.get(0)?,
                api_type: row.get(1)?,
                request_data: row.get(2)?,
                response_data: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;

        for row in rows {
            logs.push(row?);
        }
    }

    Ok(logs)
}

/// Get recent analyzer logs
pub fn get_recent_logs(conn: &Connection, limit: i64) -> Result<Vec<AnalyzerLog>> {
    get_logs(conn, None, limit, 0)
}

/// Count analyzer logs
pub fn count_logs(conn: &Connection, api_type: Option<&str>) -> Result<i64> {
    let count: i64 = if let Some(api_t) = api_type {
        conn.query_row(
            "SELECT COUNT(*) FROM analyzer_logs WHERE api_type = ?1",
            params![api_t],
            |row| row.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM analyzer_logs",
            [],
            |row| row.get(0),
        )?
    };
    Ok(count)
}

/// Clear old analyzer logs
pub fn clear_old_logs(conn: &Connection, days: i64) -> Result<usize> {
    let deleted = conn.execute(
        "DELETE FROM analyzer_logs WHERE created_at < datetime('now', ?1)",
        params![format!("-{} days", days)],
    )?;
    Ok(deleted)
}

/// Clear all analyzer logs
pub fn clear_all_logs(conn: &Connection) -> Result<usize> {
    let deleted = conn.execute("DELETE FROM analyzer_logs", [])?;
    Ok(deleted)
}

/// Get analyzer log statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerLogStats {
    pub total: i64,
    pub placeorder: i64,
    pub cancelorder: i64,
    pub modifyorder: i64,
    pub other: i64,
}

pub fn get_stats(conn: &Connection) -> Result<AnalyzerLogStats> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM analyzer_logs",
        [],
        |row| row.get(0),
    )?;

    let placeorder: i64 = conn.query_row(
        "SELECT COUNT(*) FROM analyzer_logs WHERE api_type = 'placeorder'",
        [],
        |row| row.get(0),
    )?;

    let cancelorder: i64 = conn.query_row(
        "SELECT COUNT(*) FROM analyzer_logs WHERE api_type = 'cancelorder'",
        [],
        |row| row.get(0),
    )?;

    let modifyorder: i64 = conn.query_row(
        "SELECT COUNT(*) FROM analyzer_logs WHERE api_type = 'modifyorder'",
        [],
        |row| row.get(0),
    )?;

    let other = total - placeorder - cancelorder - modifyorder;

    Ok(AnalyzerLogStats {
        total,
        placeorder,
        cancelorder,
        modifyorder,
        other,
    })
}
