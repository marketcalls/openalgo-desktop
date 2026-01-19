//! Traffic Logs Database Module
//!
//! Tracks HTTP request traffic for monitoring and security.
//! Includes IP banning, 404 tracking, and invalid API key tracking.

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

// ============================================================================
// Traffic Logs
// ============================================================================

/// Traffic log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficLog {
    pub id: i64,
    pub timestamp: String,
    pub client_ip: String,
    pub method: String,
    pub path: String,
    pub status_code: i32,
    pub duration_ms: f64,
    pub host: Option<String>,
    pub error: Option<String>,
}

/// Log a request
pub fn log_request(
    conn: &Connection,
    client_ip: &str,
    method: &str,
    path: &str,
    status_code: i32,
    duration_ms: f64,
    host: Option<&str>,
    error: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO traffic_logs (timestamp, client_ip, method, path, status_code, duration_ms, host, error)
         VALUES (datetime('now'), ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![client_ip, method, path, status_code, duration_ms, host, error],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get recent traffic logs
pub fn get_recent_logs(conn: &Connection, limit: i64) -> Result<Vec<TrafficLog>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, client_ip, method, path, status_code, duration_ms, host, error
         FROM traffic_logs
         ORDER BY timestamp DESC
         LIMIT ?1"
    )?;

    let rows = stmt.query_map(params![limit], |row| {
        Ok(TrafficLog {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            client_ip: row.get(2)?,
            method: row.get(3)?,
            path: row.get(4)?,
            status_code: row.get(5)?,
            duration_ms: row.get(6)?,
            host: row.get(7)?,
            error: row.get(8)?,
        })
    })?;

    rows.collect()
}

/// Traffic statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficStats {
    pub total_requests: i64,
    pub error_requests: i64,
    pub avg_duration: f64,
    pub requests_by_status: std::collections::HashMap<i32, i64>,
    pub requests_by_method: std::collections::HashMap<String, i64>,
}

/// Get traffic statistics
pub fn get_stats(conn: &Connection) -> Result<TrafficStats> {
    let total_requests: i64 = conn.query_row(
        "SELECT COUNT(*) FROM traffic_logs",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    let error_requests: i64 = conn.query_row(
        "SELECT COUNT(*) FROM traffic_logs WHERE status_code >= 400",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    let avg_duration: f64 = conn.query_row(
        "SELECT COALESCE(AVG(duration_ms), 0) FROM traffic_logs",
        [],
        |row| row.get(0),
    ).unwrap_or(0.0);

    // Requests by status code
    let mut requests_by_status = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT status_code, COUNT(*) FROM traffic_logs GROUP BY status_code")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i32>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            if let Ok((status, count)) = row {
                requests_by_status.insert(status, count);
            }
        }
    }

    // Requests by method
    let mut requests_by_method = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT method, COUNT(*) FROM traffic_logs GROUP BY method")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            if let Ok((method, count)) = row {
                requests_by_method.insert(method, count);
            }
        }
    }

    Ok(TrafficStats {
        total_requests,
        error_requests,
        avg_duration,
        requests_by_status,
        requests_by_method,
    })
}

/// Clear old traffic logs
pub fn clear_old_logs(conn: &Connection, days: i64) -> Result<usize> {
    let deleted = conn.execute(
        "DELETE FROM traffic_logs WHERE timestamp < datetime('now', ?1)",
        params![format!("-{} days", days)],
    )?;
    Ok(deleted)
}

// ============================================================================
// IP Bans
// ============================================================================

/// IP ban entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IPBan {
    pub id: i64,
    pub ip_address: String,
    pub ban_reason: Option<String>,
    pub ban_count: i32,
    pub banned_at: String,
    pub expires_at: Option<String>,
    pub is_permanent: bool,
    pub created_by: String,
}

/// Check if IP is banned
pub fn is_ip_banned(conn: &Connection, ip_address: &str) -> Result<bool> {
    // First check if IP exists and if it's permanent
    let ban: Option<(bool, Option<String>)> = conn.query_row(
        "SELECT is_permanent, expires_at FROM ip_bans WHERE ip_address = ?1",
        params![ip_address],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).ok();

    if let Some((is_permanent, expires_at)) = ban {
        if is_permanent {
            return Ok(true);
        }
        if expires_at.is_some() {
            // Use SQLite for datetime comparison to avoid timezone issues
            let is_expired: bool = conn.query_row(
                "SELECT expires_at < datetime('now') FROM ip_bans WHERE ip_address = ?1",
                params![ip_address],
                |row| row.get(0),
            ).unwrap_or(true);

            if !is_expired {
                return Ok(true);
            } else {
                // Ban expired, remove it
                conn.execute("DELETE FROM ip_bans WHERE ip_address = ?1", params![ip_address])?;
            }
        }
    }

    Ok(false)
}

/// Ban an IP address
pub fn ban_ip(
    conn: &Connection,
    ip_address: &str,
    reason: &str,
    duration_hours: Option<i64>,
    permanent: bool,
    created_by: &str,
) -> Result<bool> {
    // Never ban localhost
    if ip_address == "127.0.0.1" || ip_address == "::1" || ip_address == "localhost" {
        return Ok(false);
    }

    let expires = if permanent {
        None
    } else {
        duration_hours.map(|h| format!("+{} hours", h))
    };

    // Use UPSERT to atomically insert or update, avoiding race conditions
    // ban_count is incremented atomically using COALESCE to handle new rows
    conn.execute(
        "INSERT INTO ip_bans (ip_address, ban_reason, ban_count, banned_at, expires_at, is_permanent, created_by)
         VALUES (?1, ?2, 1, datetime('now'), datetime('now', ?3), ?4, ?5)
         ON CONFLICT(ip_address) DO UPDATE SET
             ban_count = ban_count + 1,
             ban_reason = excluded.ban_reason,
             banned_at = datetime('now'),
             expires_at = CASE
                 WHEN ban_count + 1 >= 5 OR excluded.is_permanent = 1 THEN NULL
                 ELSE datetime('now', excluded.expires_at)
             END,
             is_permanent = CASE
                 WHEN ban_count + 1 >= 5 OR excluded.is_permanent = 1 THEN 1
                 ELSE excluded.is_permanent
             END",
        params![ip_address, reason, expires, permanent, created_by],
    )?;

    Ok(true)
}

/// Unban an IP address
pub fn unban_ip(conn: &Connection, ip_address: &str) -> Result<bool> {
    let deleted = conn.execute(
        "DELETE FROM ip_bans WHERE ip_address = ?1",
        params![ip_address],
    )?;
    Ok(deleted > 0)
}

/// Get all IP bans
pub fn get_all_bans(conn: &Connection) -> Result<Vec<IPBan>> {
    // Clean up expired bans first
    conn.execute(
        "DELETE FROM ip_bans WHERE is_permanent = 0 AND expires_at < datetime('now')",
        [],
    )?;

    let mut stmt = conn.prepare(
        "SELECT id, ip_address, ban_reason, ban_count, banned_at, expires_at, is_permanent, created_by
         FROM ip_bans"
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(IPBan {
            id: row.get(0)?,
            ip_address: row.get(1)?,
            ban_reason: row.get(2)?,
            ban_count: row.get(3)?,
            banned_at: row.get(4)?,
            expires_at: row.get(5)?,
            is_permanent: row.get(6)?,
            created_by: row.get(7)?,
        })
    })?;

    rows.collect()
}

// ============================================================================
// 404 Error Tracking
// ============================================================================

/// Track 404 error
pub fn track_404(conn: &Connection, ip_address: &str, path: &str) -> Result<()> {
    // Check if tracking exists
    let existing: Option<(i32, String)> = conn.query_row(
        "SELECT error_count, paths_attempted FROM error_404_tracker WHERE ip_address = ?1",
        params![ip_address],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).ok();

    if let Some((count, paths)) = existing {
        // Update existing tracker
        let mut paths_vec: Vec<String> = serde_json::from_str(&paths).unwrap_or_default();
        if !paths_vec.contains(&path.to_string()) {
            paths_vec.push(path.to_string());
            if paths_vec.len() > 50 {
                paths_vec = paths_vec[paths_vec.len()-50..].to_vec();
            }
        }

        conn.execute(
            "UPDATE error_404_tracker SET error_count = ?1, last_error_at = datetime('now'),
             paths_attempted = ?2 WHERE ip_address = ?3",
            params![count + 1, serde_json::to_string(&paths_vec).unwrap_or_default(), ip_address],
        )?;
    } else {
        // Create new tracker
        let paths = serde_json::to_string(&vec![path]).unwrap_or_default();
        conn.execute(
            "INSERT INTO error_404_tracker (ip_address, error_count, first_error_at, last_error_at, paths_attempted)
             VALUES (?1, 1, datetime('now'), datetime('now'), ?2)",
            params![ip_address, paths],
        )?;
    }

    Ok(())
}

/// Get suspicious IPs with high 404 counts
pub fn get_suspicious_404_ips(conn: &Connection, min_errors: i32) -> Result<Vec<(String, i32, String)>> {
    // Clean up old entries (older than 24 hours)
    conn.execute(
        "DELETE FROM error_404_tracker WHERE first_error_at < datetime('now', '-1 day')",
        [],
    )?;

    let mut stmt = conn.prepare(
        "SELECT ip_address, error_count, paths_attempted
         FROM error_404_tracker
         WHERE error_count >= ?1
         ORDER BY error_count DESC"
    )?;

    let rows = stmt.query_map(params![min_errors], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?, row.get::<_, String>(2)?))
    })?;

    rows.collect()
}

// ============================================================================
// Invalid API Key Tracking
// ============================================================================

/// Track invalid API key attempt
pub fn track_invalid_api_key(conn: &Connection, ip_address: &str, api_key_hash: Option<&str>) -> Result<()> {
    let existing: Option<(i32, String)> = conn.query_row(
        "SELECT attempt_count, api_keys_tried FROM invalid_api_key_tracker WHERE ip_address = ?1",
        params![ip_address],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).ok();

    if let Some((count, keys)) = existing {
        let mut keys_vec: Vec<String> = serde_json::from_str(&keys).unwrap_or_default();
        if let Some(hash) = api_key_hash {
            if !keys_vec.contains(&hash.to_string()) {
                keys_vec.push(hash.to_string());
                if keys_vec.len() > 20 {
                    keys_vec = keys_vec[keys_vec.len()-20..].to_vec();
                }
            }
        }

        conn.execute(
            "UPDATE invalid_api_key_tracker SET attempt_count = ?1, last_attempt_at = datetime('now'),
             api_keys_tried = ?2 WHERE ip_address = ?3",
            params![count + 1, serde_json::to_string(&keys_vec).unwrap_or_default(), ip_address],
        )?;
    } else {
        let keys = if let Some(hash) = api_key_hash {
            serde_json::to_string(&vec![hash]).unwrap_or_default()
        } else {
            "[]".to_string()
        };

        conn.execute(
            "INSERT INTO invalid_api_key_tracker (ip_address, attempt_count, first_attempt_at, last_attempt_at, api_keys_tried)
             VALUES (?1, 1, datetime('now'), datetime('now'), ?2)",
            params![ip_address, keys],
        )?;
    }

    Ok(())
}

/// Get suspicious API users
pub fn get_suspicious_api_users(conn: &Connection, min_attempts: i32) -> Result<Vec<(String, i32)>> {
    // Clean up old entries
    conn.execute(
        "DELETE FROM invalid_api_key_tracker WHERE first_attempt_at < datetime('now', '-1 day')",
        [],
    )?;

    let mut stmt = conn.prepare(
        "SELECT ip_address, attempt_count
         FROM invalid_api_key_tracker
         WHERE attempt_count >= ?1
         ORDER BY attempt_count DESC"
    )?;

    let rows = stmt.query_map(params![min_attempts], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
    })?;

    rows.collect()
}
