//! Monitoring tables in `logs.db`: traffic, IP bans, 404 and invalid API key
//! trackers, login activity, browser and server error reports, health
//! samples and alerts, and API latency. Column names follow the web's
//! `traffic_db`, `auth_db.LoginAttempt`, `health_db` and `latency_db`.
//!
//! Every table is bounded: rows past a retention age are deleted and each
//! table has a hard row cap (see [`RETENTION`]), so the file cannot grow for
//! the life of the install. Timestamps are stored as UTC
//! `YYYY-MM-DD HH:MM:SS` text.
//!
//! Nothing here stores a request body, a query string or an API key.

use crate::error::Result;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub const TS_FMT: &str = "%Y-%m-%d %H:%M:%S";

pub fn ts(t: DateTime<Utc>) -> String {
    t.format(TS_FMT).to_string()
}

pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, TS_FMT)
        .ok()
        .map(|n| n.and_utc())
}

/// Retention per table: (table, timestamp column, max age in days, row cap).
pub const RETENTION: &[(&str, &str, i64, i64)] = &[
    ("traffic_logs", "timestamp", 30, 100_000),
    ("error_logs", "ts", 30, 5_000),
    ("health_metrics", "timestamp", 7, 20_000),
    ("login_attempts", "timestamp", 365, 10_000),
    ("error_404_tracker", "first_error_at", 1, 10_000),
    ("invalid_api_key_tracker", "first_attempt_at", 1, 10_000),
];

/// Latency rows of these types are orders and kept (up to the cap); data
/// queries are purged after a week (web `KEEP_FOREVER_TYPES`).
pub const LATENCY_ORDER_TYPES: &[&str] = &[
    "PLACE",
    "SMART",
    "MODIFY",
    "CANCEL",
    "CLOSE",
    "CANCEL_ALL",
    "BASKET",
    "SPLIT",
    "OPTIONS",
    "OPTIONS_MULTI",
    "GTT_PLACE",
    "GTT_MODIFY",
    "GTT_CANCEL",
];
pub const LATENCY_ROW_CAP: i64 = 200_000;
pub const HEALTH_ALERT_CAP: i64 = 1_000;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS traffic_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    client_ip TEXT NOT NULL,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    status_code INTEGER NOT NULL,
    duration_ms REAL NOT NULL,
    host TEXT,
    error TEXT,
    user_id INTEGER
);
CREATE INDEX IF NOT EXISTS idx_traffic_logs_timestamp ON traffic_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_traffic_logs_path ON traffic_logs(path);
CREATE TABLE IF NOT EXISTS ip_bans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ip_address TEXT NOT NULL UNIQUE,
    ban_reason TEXT,
    ban_count INTEGER NOT NULL DEFAULT 1,
    banned_at TEXT NOT NULL,
    expires_at TEXT,
    is_permanent INTEGER NOT NULL DEFAULT 0,
    created_by TEXT NOT NULL DEFAULT 'system'
);
CREATE TABLE IF NOT EXISTS error_404_tracker (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ip_address TEXT NOT NULL UNIQUE,
    error_count INTEGER NOT NULL DEFAULT 1,
    first_error_at TEXT NOT NULL,
    last_error_at TEXT NOT NULL,
    paths_attempted TEXT
);
CREATE TABLE IF NOT EXISTS invalid_api_key_tracker (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ip_address TEXT NOT NULL UNIQUE,
    attempt_count INTEGER NOT NULL DEFAULT 1,
    first_attempt_at TEXT NOT NULL,
    last_attempt_at TEXT NOT NULL,
    api_keys_tried TEXT
);
CREATE TABLE IF NOT EXISTS login_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL,
    ip_address TEXT,
    device_info TEXT,
    status TEXT NOT NULL,
    login_type TEXT,
    broker TEXT,
    failure_reason TEXT,
    timestamp TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_login_attempts_timestamp ON login_attempts(timestamp);
CREATE TABLE IF NOT EXISTS error_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    level TEXT NOT NULL,
    logger TEXT,
    module TEXT,
    file TEXT,
    message TEXT NOT NULL,
    exception TEXT,
    request TEXT
);
CREATE INDEX IF NOT EXISTS idx_error_logs_ts ON error_logs(ts);
CREATE TABLE IF NOT EXISTS health_metrics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    fd_count INTEGER,
    fd_limit INTEGER,
    fd_usage_percent REAL,
    fd_available INTEGER,
    fd_status TEXT,
    memory_rss_mb REAL,
    memory_vms_mb REAL,
    memory_percent REAL,
    memory_available_mb REAL,
    memory_swap_mb REAL,
    memory_status TEXT,
    db_connections_total INTEGER,
    db_connections TEXT,
    db_status TEXT,
    ws_connections_total INTEGER,
    ws_connections TEXT,
    ws_total_symbols INTEGER,
    ws_status TEXT,
    thread_count INTEGER,
    stuck_threads INTEGER,
    thread_details TEXT,
    thread_status TEXT,
    process_details TEXT,
    db_sizes TEXT,
    overall_status TEXT
);
CREATE INDEX IF NOT EXISTS idx_health_metrics_timestamp ON health_metrics(timestamp);
CREATE TABLE IF NOT EXISTS health_alerts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    alert_type TEXT,
    severity TEXT,
    metric_name TEXT,
    metric_value REAL,
    threshold_value REAL,
    message TEXT,
    acknowledged INTEGER NOT NULL DEFAULT 0,
    acknowledged_at TEXT,
    resolved INTEGER NOT NULL DEFAULT 0,
    resolved_at TEXT
);
CREATE TABLE IF NOT EXISTS order_latency (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    order_id TEXT NOT NULL,
    user_id INTEGER,
    broker TEXT,
    symbol TEXT,
    order_type TEXT,
    rtt_ms REAL,
    validation_latency_ms REAL,
    response_latency_ms REAL,
    overhead_ms REAL,
    total_latency_ms REAL NOT NULL,
    request_body TEXT,
    response_body TEXT,
    status TEXT,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_order_latency_timestamp ON order_latency(timestamp);
CREATE INDEX IF NOT EXISTS idx_order_latency_broker ON order_latency(broker);
";

fn applied(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM migrations WHERE name = ?1)",
        [name],
        |r| r.get(0),
    )?)
}

/// Paths whose next segment is the credential itself (the strategy webhook
/// token, the Chartink webhook id).
const SECRET_PATH_PREFIXES: [&str; 2] = ["/strategy/webhook/", "/chartink/webhook/"];
pub const REDACTED: &str = "<redacted>";

/// `text` (a path, or a stored list of paths) with every webhook secret
/// segment replaced by `<redacted>`. Security review S-06: the traffic log
/// and the 404 tracker keep the route, never the secret.
pub fn redact_secret_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let next = SECRET_PATH_PREFIXES
            .iter()
            .filter_map(|p| rest.find(p).map(|i| (i, *p)))
            .min_by_key(|(i, _)| *i);
        let Some((i, prefix)) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i + prefix.len()]);
        rest = &rest[i + prefix.len()..];
        let end = rest
            .find(|c: char| {
                matches!(c, ',' | '"' | '\'' | ']' | '/' | '?' | ';') || c.is_whitespace()
            })
            .unwrap_or(rest.len());
        if end > 0 {
            out.push_str(REDACTED);
        }
        rest = &rest[end..];
    }
}

/// Migration `011_redact_webhook_paths` of `logs.db`: scrub secrets that
/// earlier builds wrote. Idempotent and recorded once.
fn redact_stored_paths(conn: &Connection) -> Result<()> {
    if applied(conn, "011_redact_webhook_paths")? {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let r = (|| -> Result<()> {
        for (table, column) in [
            ("traffic_logs", "path"),
            ("error_404_tracker", "paths_attempted"),
        ] {
            let rows: Vec<(i64, String)> = {
                let mut st = conn.prepare(&format!(
                    "SELECT id, {c} FROM {t} WHERE {c} LIKE '%/strategy/webhook/%' \
                     OR {c} LIKE '%/chartink/webhook/%'",
                    c = column,
                    t = table
                ))?;
                let v = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                v
            };
            for (id, text) in rows {
                let clean = redact_secret_paths(&text);
                if clean != text {
                    conn.execute(
                        &format!("UPDATE {} SET {} = ?1 WHERE id = ?2", table, column),
                        rusqlite::params![clean, id],
                    )?;
                }
            }
        }
        conn.execute(
            "INSERT OR IGNORE INTO migrations (name) VALUES ('011_redact_webhook_paths')",
            [],
        )?;
        Ok(())
    })();
    match r {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Schema for the monitoring tables (idempotent). Runs on every open.
pub fn migrate(conn: &Connection) -> Result<()> {
    create_tables(conn)?;
    redact_stored_paths(conn)
}

fn create_tables(conn: &Connection) -> Result<()> {
    if applied(conn, "010_monitoring_tables")? {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let r = conn.execute_batch(SCHEMA).and_then(|_| {
        conn.execute(
            "INSERT OR IGNORE INTO migrations (name) VALUES ('010_monitoring_tables')",
            [],
        )
        .map(|_| ())
    });
    match r {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e.into())
        }
    }
}

fn main_has_table(main: &Connection, table: &str) -> Result<bool> {
    Ok(main.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )?)
}

/// Copy monitoring rows that earlier builds kept in the main database
/// (`traffic_logs`, `ip_bans`, `latency_logs`) into `logs.db`, once. The
/// newest rows within each table's cap are kept.
pub fn import_from_main(logs: &Connection, main: &Connection) -> Result<()> {
    const NAME: &str = "011_import_main_monitoring";
    if applied(logs, NAME)? {
        return Ok(());
    }
    logs.execute_batch("BEGIN IMMEDIATE")?;
    let r = (|| -> Result<()> {
        if main_has_table(main, "traffic_logs")? {
            let mut st = main.prepare(
                "SELECT timestamp, client_ip, method, path, status_code, duration_ms, host, error
                 FROM traffic_logs ORDER BY id DESC LIMIT 100000",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, f64>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for row in rows.into_iter().rev() {
                logs.execute(
                    "INSERT INTO traffic_logs (timestamp, client_ip, method, path, status_code, duration_ms, host, error)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![row.0, row.1, row.2, strip_query(&row.3), row.4, row.5, row.6, row.7],
                )?;
            }
        }
        if main_has_table(main, "ip_bans")? {
            let mut st = main.prepare(
                "SELECT ip_address, ban_reason, ban_count, banned_at, expires_at, is_permanent, created_by
                 FROM ip_bans",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<bool>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for b in rows {
                logs.execute(
                    "INSERT OR IGNORE INTO ip_bans (ip_address, ban_reason, ban_count, banned_at, expires_at, is_permanent, created_by)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        b.0,
                        b.1,
                        b.2.unwrap_or(1),
                        b.3,
                        b.4,
                        b.5.unwrap_or(false),
                        b.6.unwrap_or_else(|| "system".into())
                    ],
                )?;
            }
        }
        if main_has_table(main, "latency_logs")? {
            let mut st = main.prepare(
                "SELECT timestamp, order_id, broker, symbol, order_type, rtt_ms, validation_ms,
                        broker_response_ms, overhead_ms, total_ms, status, error
                 FROM latency_logs ORDER BY id DESC LIMIT 200000",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<f64>>(5)?,
                        r.get::<_, Option<f64>>(6)?,
                        r.get::<_, Option<f64>>(7)?,
                        r.get::<_, Option<f64>>(8)?,
                        r.get::<_, f64>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<String>>(11)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for l in rows.into_iter().rev() {
                logs.execute(
                    "INSERT INTO order_latency (timestamp, order_id, broker, symbol, order_type, rtt_ms,
                        validation_latency_ms, response_latency_ms, overhead_ms, total_latency_ms, status, error)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![l.0, l.1, l.2, l.3, l.4, l.5, l.6, l.7, l.8, l.9, l.10, l.11],
                )?;
            }
        }
        logs.execute(
            "INSERT OR IGNORE INTO migrations (name) VALUES (?1)",
            [NAME],
        )?;
        Ok(())
    })();
    match r {
        Ok(()) => {
            logs.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = logs.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Path without query string or fragment (queries may carry credentials).
pub fn strip_query(path: &str) -> String {
    path.split(['?', '#']).next().unwrap_or("").to_string()
}

fn cap_text(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Apply [`RETENTION`] and the latency rules. Returns rows deleted.
pub fn purge(conn: &Connection, now: DateTime<Utc>) -> Result<usize> {
    let mut deleted = 0;
    for (table, col, days, cap) in RETENTION {
        let cutoff = ts(now - Duration::days(*days));
        deleted += conn.execute(&format!("DELETE FROM {table} WHERE {col} < ?1"), [cutoff])?;
        deleted += conn.execute(
            &format!(
                "DELETE FROM {table} WHERE id <= (SELECT id FROM {table} ORDER BY id DESC LIMIT 1 OFFSET ?1)"
            ),
            [cap],
        )?;
    }
    let placeholders = LATENCY_ORDER_TYPES
        .iter()
        .map(|t| format!("'{}'", t))
        .collect::<Vec<_>>()
        .join(",");
    deleted += conn.execute(
        &format!(
            "DELETE FROM order_latency WHERE timestamp < ?1 AND COALESCE(order_type, '') NOT IN ({placeholders})"
        ),
        [ts(now - Duration::days(7))],
    )?;
    deleted += conn.execute(
        "DELETE FROM order_latency WHERE id <= (SELECT id FROM order_latency ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [LATENCY_ROW_CAP],
    )?;
    deleted += conn.execute(
        "DELETE FROM health_alerts WHERE resolved = 1 AND timestamp < ?1",
        [ts(now - Duration::days(7))],
    )?;
    deleted += conn.execute(
        "DELETE FROM health_alerts WHERE id <= (SELECT id FROM health_alerts ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [HEALTH_ALERT_CAP],
    )?;
    deleted += conn.execute(
        "DELETE FROM ip_bans WHERE is_permanent = 0 AND expires_at IS NOT NULL AND expires_at < ?1",
        [ts(now)],
    )?;
    Ok(deleted)
}

pub fn count(conn: &Connection, table: &str) -> Result<i64> {
    Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
}

// ------------------------------------------------------------------ traffic

#[derive(Debug, Clone)]
pub struct TrafficRow {
    pub timestamp: String,
    pub client_ip: String,
    pub method: String,
    pub path: String,
    pub status_code: i64,
    pub duration_ms: f64,
    pub host: Option<String>,
    pub error: Option<String>,
}

pub fn insert_traffic(conn: &Connection, r: &TrafficRow) -> Result<()> {
    conn.execute(
        "INSERT INTO traffic_logs (timestamp, client_ip, method, path, status_code, duration_ms, host, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            r.timestamp,
            cap_text(&r.client_ip, 50),
            cap_text(&r.method, 10),
            cap_text(&strip_query(&r.path), 500),
            r.status_code,
            r.duration_ms,
            r.host.as_deref().map(|h| cap_text(h, 500)),
            r.error.as_deref().map(|e| cap_text(e, 500)),
        ],
    )?;
    Ok(())
}

fn traffic_from_row(r: &rusqlite::Row) -> rusqlite::Result<TrafficRow> {
    Ok(TrafficRow {
        timestamp: r.get(0)?,
        client_ip: r.get(1)?,
        method: r.get(2)?,
        path: r.get(3)?,
        status_code: r.get(4)?,
        duration_ms: r.get(5)?,
        host: r.get(6)?,
        error: r.get(7)?,
    })
}

pub fn recent_traffic(conn: &Connection, limit: Option<i64>) -> Result<Vec<TrafficRow>> {
    let mut st = conn.prepare(
        "SELECT timestamp, client_ip, method, path, status_code, duration_ms, host, error
         FROM traffic_logs ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = st
        .query_map([limit.unwrap_or(-1)], traffic_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(total, errors, avg_duration)` for paths matching `like` (all when None).
pub fn traffic_summary(conn: &Connection, like: Option<&str>) -> Result<(i64, i64, f64)> {
    let pat = like.unwrap_or("%");
    Ok(conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END), 0),
                COALESCE(AVG(duration_ms), 0)
         FROM traffic_logs WHERE path LIKE ?1",
        [pat],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

/// Distinct client addresses seen with a Host header containing `host`.
pub fn traffic_ips_for_host(conn: &Connection, host: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT DISTINCT client_ip FROM traffic_logs WHERE host LIKE ?1 ESCAPE '\\' LIMIT 1000",
    )?;
    let esc = host
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let rows = st
        .query_map([format!("%{}%", esc)], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ------------------------------------------------------------------ bans

#[derive(Debug, Clone)]
pub struct BanRow {
    pub ip_address: String,
    pub ban_reason: Option<String>,
    pub ban_count: i64,
    pub banned_at: String,
    pub expires_at: Option<String>,
    pub is_permanent: bool,
    pub created_by: String,
}

pub fn is_loopback_ip(ip: &str) -> bool {
    matches!(ip, "127.0.0.1" | "::1" | "localhost")
        || ip
            .parse::<std::net::IpAddr>()
            .map(|a| a.is_loopback())
            .unwrap_or(false)
}

/// Addresses that are never banned: loopback (the trader's own machine), and
/// the internal identities in the discard-only `100::/64` block: the shared
/// identity of every caller behind a tunnel or proxy (banning it would cut
/// off the trader's own TradingView and Chartink alerts) and the MCP
/// dispatcher (security review S-03).
pub fn never_banned(ip: &str) -> bool {
    is_loopback_ip(ip)
        || matches!(
            ip.parse::<std::net::IpAddr>(),
            Ok(std::net::IpAddr::V6(v)) if v.segments()[..4] == [0x100, 0, 0, 0]
        )
}

/// Ban (or re-ban) an address. A fifth ban, or `repeat_limit` bans, makes it
/// permanent as on the web. Loopback is never banned.
#[allow(clippy::too_many_arguments)]
pub fn ban_ip(
    conn: &Connection,
    ip: &str,
    reason: &str,
    duration_hours: Option<i64>,
    permanent: bool,
    created_by: &str,
    now: DateTime<Utc>,
    repeat_limit: i64,
) -> Result<bool> {
    if never_banned(ip) {
        return Ok(false);
    }
    let existing: Option<i64> = conn
        .query_row(
            "SELECT ban_count FROM ip_bans WHERE ip_address = ?1",
            [ip],
            |r| r.get(0),
        )
        .optional()?;
    let count = existing.map(|c| c + 1).unwrap_or(1);
    let permanent = permanent || duration_hours.is_none() || count > repeat_limit.max(1);
    let expires = if permanent {
        None
    } else {
        duration_hours.map(|h| ts(now + Duration::hours(h)))
    };
    conn.execute(
        "INSERT INTO ip_bans (ip_address, ban_reason, ban_count, banned_at, expires_at, is_permanent, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(ip_address) DO UPDATE SET ban_reason = excluded.ban_reason,
             ban_count = excluded.ban_count, banned_at = excluded.banned_at,
             expires_at = excluded.expires_at, is_permanent = excluded.is_permanent,
             created_by = excluded.created_by",
        params![
            ip,
            cap_text(reason, 200),
            count,
            ts(now),
            expires,
            permanent,
            cap_text(created_by, 50)
        ],
    )?;
    Ok(true)
}

pub fn unban_ip(conn: &Connection, ip: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM ip_bans WHERE ip_address = ?1", [ip])? > 0)
}

pub fn all_bans(conn: &Connection, now: DateTime<Utc>) -> Result<Vec<BanRow>> {
    conn.execute(
        "DELETE FROM ip_bans WHERE is_permanent = 0 AND expires_at IS NOT NULL AND expires_at < ?1",
        [ts(now)],
    )?;
    let mut st = conn.prepare(
        "SELECT ip_address, ban_reason, ban_count, banned_at, expires_at, is_permanent, created_by
         FROM ip_bans ORDER BY id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(BanRow {
                ip_address: r.get(0)?,
                ban_reason: r.get(1)?,
                ban_count: r.get(2)?,
                banned_at: r.get(3)?,
                expires_at: r.get(4)?,
                is_permanent: r.get(5)?,
                created_by: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ------------------------------------------------------- 404 / invalid key

#[derive(Debug, Clone)]
pub struct TrackerRow {
    pub ip_address: String,
    pub count: i64,
    pub first_at: String,
    pub last_at: String,
    pub detail: Option<String>,
}

/// Count a 404 for `ip`, remembering up to 50 distinct paths. A tracker
/// older than a day starts over (web `_track_404_locked`). Returns the count.
pub fn track_404(conn: &Connection, ip: &str, path: &str, now: DateTime<Utc>) -> Result<i64> {
    let path = cap_text(&strip_query(path), 200);
    let row: Option<(i64, String, Option<String>)> = conn
        .query_row(
            "SELECT error_count, first_error_at, paths_attempted FROM error_404_tracker WHERE ip_address = ?1",
            [ip],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let stale = |first: &str| {
        parse_ts(first)
            .map(|f| now - f >= Duration::days(1))
            .unwrap_or(true)
    };
    match row {
        Some((n, first, paths)) if !stale(&first) => {
            let mut v: Vec<String> = paths
                .and_then(|p| serde_json::from_str(&p).ok())
                .unwrap_or_default();
            if !v.contains(&path) {
                v.push(path);
                if v.len() > 50 {
                    v.drain(..v.len() - 50);
                }
            }
            conn.execute(
                "UPDATE error_404_tracker SET error_count = ?1, last_error_at = ?2, paths_attempted = ?3
                 WHERE ip_address = ?4",
                params![n + 1, ts(now), Value::from(v).to_string(), ip],
            )?;
            Ok(n + 1)
        }
        _ => {
            conn.execute(
                "INSERT INTO error_404_tracker (ip_address, error_count, first_error_at, last_error_at, paths_attempted)
                 VALUES (?1, 1, ?2, ?2, ?3)
                 ON CONFLICT(ip_address) DO UPDATE SET error_count = 1, first_error_at = excluded.first_error_at,
                     last_error_at = excluded.last_error_at, paths_attempted = excluded.paths_attempted",
                params![ip, ts(now), json!([path]).to_string()],
            )?;
            Ok(1)
        }
    }
}

/// Count an invalid API key attempt for `ip`. The key itself is never
/// stored (the web keeps hashes; the desktop keeps nothing).
pub fn track_invalid_api_key(conn: &Connection, ip: &str, now: DateTime<Utc>) -> Result<i64> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT attempt_count, first_attempt_at FROM invalid_api_key_tracker WHERE ip_address = ?1",
            [ip],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let fresh = row
        .as_ref()
        .and_then(|(_, f)| parse_ts(f))
        .map(|f| now - f < Duration::days(1))
        .unwrap_or(false);
    match row {
        Some((n, _)) if fresh => {
            conn.execute(
                "UPDATE invalid_api_key_tracker SET attempt_count = ?1, last_attempt_at = ?2 WHERE ip_address = ?3",
                params![n + 1, ts(now), ip],
            )?;
            Ok(n + 1)
        }
        _ => {
            conn.execute(
                "INSERT INTO invalid_api_key_tracker (ip_address, attempt_count, first_attempt_at, last_attempt_at, api_keys_tried)
                 VALUES (?1, 1, ?2, ?2, '[]')
                 ON CONFLICT(ip_address) DO UPDATE SET attempt_count = 1,
                     first_attempt_at = excluded.first_attempt_at, last_attempt_at = excluded.last_attempt_at",
                params![ip, ts(now)],
            )?;
            Ok(1)
        }
    }
}

fn trackers(
    conn: &Connection,
    sql: &str,
    min: i64,
    now: DateTime<Utc>,
    first_col: &str,
    table: &str,
) -> Result<Vec<TrackerRow>> {
    conn.execute(
        &format!("DELETE FROM {table} WHERE {first_col} < ?1"),
        [ts(now - Duration::days(1))],
    )?;
    let mut st = conn.prepare(sql)?;
    let rows = st
        .query_map([min], |r| {
            Ok(TrackerRow {
                ip_address: r.get(0)?,
                count: r.get(1)?,
                first_at: r.get(2)?,
                last_at: r.get(3)?,
                detail: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn suspicious_404(conn: &Connection, min: i64, now: DateTime<Utc>) -> Result<Vec<TrackerRow>> {
    trackers(
        conn,
        "SELECT ip_address, error_count, first_error_at, last_error_at, paths_attempted
         FROM error_404_tracker WHERE error_count >= ?1 ORDER BY error_count DESC LIMIT 1000",
        min,
        now,
        "first_error_at",
        "error_404_tracker",
    )
}

pub fn suspicious_api(conn: &Connection, min: i64, now: DateTime<Utc>) -> Result<Vec<TrackerRow>> {
    trackers(
        conn,
        "SELECT ip_address, attempt_count, first_attempt_at, last_attempt_at, api_keys_tried
         FROM invalid_api_key_tracker WHERE attempt_count >= ?1 ORDER BY attempt_count DESC LIMIT 1000",
        min,
        now,
        "first_attempt_at",
        "invalid_api_key_tracker",
    )
}

pub fn clear_404(conn: &Connection, ip: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM error_404_tracker WHERE ip_address = ?1", [ip])? > 0)
}

// ------------------------------------------------------------ login activity

#[derive(Debug, Clone, Default)]
pub struct LoginAttempt {
    pub username: String,
    pub ip_address: Option<String>,
    pub device_info: Option<String>,
    pub status: String,
    pub login_type: Option<String>,
    pub broker: Option<String>,
    pub failure_reason: Option<String>,
}

pub fn insert_login_attempt(conn: &Connection, a: &LoginAttempt, now: DateTime<Utc>) -> Result<()> {
    conn.execute(
        "INSERT INTO login_attempts (username, ip_address, device_info, status, login_type, broker, failure_reason, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            cap_text(&a.username, 255),
            a.ip_address,
            a.device_info.as_deref().map(|d| cap_text(d, 500)),
            cap_text(&a.status, 20),
            a.login_type,
            a.broker,
            a.failure_reason.as_deref().map(|d| cap_text(d, 255)),
            ts(now)
        ],
    )?;
    Ok(())
}

pub fn login_attempts(
    conn: &Connection,
    limit: i64,
    status: Option<&str>,
) -> Result<Vec<(LoginAttempt, String)>> {
    let mut st = conn.prepare(
        "SELECT username, ip_address, device_info, status, login_type, broker, failure_reason, timestamp
         FROM login_attempts WHERE (?1 IS NULL OR status = ?1) ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = st
        .query_map(params![status, limit], |r| {
            Ok((
                LoginAttempt {
                    username: r.get(0)?,
                    ip_address: r.get(1)?,
                    device_info: r.get(2)?,
                    status: r.get(3)?,
                    login_type: r.get(4)?,
                    broker: r.get(5)?,
                    failure_reason: r.get(6)?,
                },
                r.get::<_, String>(7)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn clear_login_attempts(conn: &Connection) -> Result<usize> {
    Ok(conn.execute("DELETE FROM login_attempts", [])?)
}

// ------------------------------------------------------------------ errors

#[derive(Debug, Clone, Default)]
pub struct ErrorEntry {
    pub ts: String,
    pub level: String,
    pub logger: Option<String>,
    pub module: Option<String>,
    pub file: Option<String>,
    pub message: String,
    pub exception: Option<Value>,
    pub request: Option<Value>,
}

impl ErrorEntry {
    /// The web's sanitized errors.jsonl entry: only allowed keys, capped.
    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("ts".into(), json!(self.ts));
        m.insert("level".into(), json!(self.level));
        if let Some(v) = &self.logger {
            m.insert("logger".into(), json!(v));
        }
        if let Some(v) = &self.module {
            m.insert("module".into(), json!(v));
        }
        if let Some(v) = &self.file {
            m.insert("file".into(), json!(v));
        }
        m.insert("message".into(), json!(cap_text(&self.message, 20_000)));
        if let Some(v) = &self.exception {
            m.insert("exception".into(), v.clone());
        }
        if let Some(v) = &self.request {
            m.insert("request".into(), v.clone());
        }
        Value::Object(m)
    }
}

pub fn insert_error(conn: &Connection, e: &ErrorEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO error_logs (ts, level, logger, module, file, message, exception, request)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            e.ts,
            cap_text(&e.level, 16),
            e.logger.as_deref().map(|s| cap_text(s, 200)),
            e.module.as_deref().map(|s| cap_text(s, 200)),
            e.file.as_deref().map(|s| cap_text(s, 300)),
            cap_text(&e.message, 40_000),
            e.exception
                .as_ref()
                .map(|v| cap_text(&v.to_string(), 20_000)),
            e.request.as_ref().map(|v| cap_text(&v.to_string(), 2_000)),
        ],
    )?;
    Ok(())
}

/// All error entries, oldest first (bounded by the table cap).
pub fn all_errors(conn: &Connection) -> Result<Vec<ErrorEntry>> {
    let mut st = conn.prepare(
        "SELECT ts, level, logger, module, file, message, exception, request FROM error_logs ORDER BY id",
    )?;
    let rows = st
        .query_map([], |r| {
            let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Value>(&s).ok());
            Ok(ErrorEntry {
                ts: r.get(0)?,
                level: r.get(1)?,
                logger: r.get(2)?,
                module: r.get(3)?,
                file: r.get(4)?,
                message: r.get(5)?,
                exception: parse(r.get(6)?),
                request: parse(r.get(7)?),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ------------------------------------------------------------------ latency

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
        .query_map([limit.unwrap_or(-1)], |r| {
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
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(broker, rtt_ms, total_ms, failed, timestamp)` for statistics.
/// `(broker, rtt_ms, total_ms, failed, timestamp)`.
pub type LatencyPoint = (Option<String>, f64, f64, bool, String);

pub fn latency_points(conn: &Connection) -> Result<Vec<LatencyPoint>> {
    let mut st = conn.prepare(
        "SELECT broker, COALESCE(rtt_ms, 0), total_latency_ms, status = 'FAILED', timestamp
         FROM order_latency ORDER BY id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, Option<bool>>(3)?.unwrap_or(false),
                r.get(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// --------------------------------------------------------- API order logs

/// One `order_logs` row: (id, api_type, request_data, response_data, created_at).
pub type OrderLogRow = (i64, String, String, String, String);

/// `order_logs` (live API calls, request bodies already without the API key)
/// created in `[from, to)` (ISO UTC text), optionally matching `search`.
/// `page` is `(offset, limit)`; None returns every match.
pub fn order_logs_page(
    conn: &Connection,
    from: &str,
    to: &str,
    search: Option<&str>,
    page: Option<(i64, i64)>,
) -> Result<(Vec<OrderLogRow>, i64)> {
    let like = search.map(|s| {
        let esc = s
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{}%", esc)
    });
    let filter = "created_at >= ?1 AND created_at < ?2 AND (?3 IS NULL
        OR api_type LIKE ?3 ESCAPE '\\' OR request_data LIKE ?3 ESCAPE '\\'
        OR response_data LIKE ?3 ESCAPE '\\')";
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM order_logs WHERE {filter}"),
        params![from, to, like],
        |r| r.get(0),
    )?;
    let (offset, limit) = page.unwrap_or((0, -1));
    let mut st = conn.prepare(&format!(
        "SELECT id, api_type, request_data, response_data, created_at FROM order_logs
         WHERE {filter} ORDER BY created_at DESC, id DESC LIMIT ?4 OFFSET ?5"
    ))?;
    let rows = st
        .query_map(params![from, to, like, limit, offset], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((rows, total))
}

// ------------------------------------------------------------------ health

#[derive(Debug, Clone, Default)]
pub struct HealthRow {
    pub id: i64,
    pub timestamp: String,
    pub fd_count: Option<i64>,
    pub fd_limit: Option<i64>,
    pub fd_usage_percent: Option<f64>,
    pub fd_status: Option<String>,
    pub memory_rss_mb: Option<f64>,
    pub memory_vms_mb: Option<f64>,
    pub memory_percent: Option<f64>,
    pub memory_available_mb: Option<f64>,
    pub memory_swap_mb: Option<f64>,
    pub memory_status: Option<String>,
    pub db_connections_total: Option<i64>,
    pub db_connections: Option<Value>,
    pub db_status: Option<String>,
    pub ws_connections_total: Option<i64>,
    pub ws_connections: Option<Value>,
    pub ws_total_symbols: Option<i64>,
    pub ws_status: Option<String>,
    pub thread_count: Option<i64>,
    pub stuck_threads: Option<i64>,
    pub thread_details: Option<Value>,
    pub thread_status: Option<String>,
    pub process_details: Option<Value>,
    pub db_sizes: Option<Value>,
    pub overall_status: Option<String>,
}

pub fn insert_health(conn: &Connection, h: &HealthRow) -> Result<i64> {
    let j = |v: &Option<Value>| v.as_ref().map(|v| v.to_string());
    conn.execute(
        "INSERT INTO health_metrics (timestamp, fd_count, fd_limit, fd_usage_percent, fd_available, fd_status,
            memory_rss_mb, memory_vms_mb, memory_percent, memory_available_mb, memory_swap_mb, memory_status,
            db_connections_total, db_connections, db_status, ws_connections_total, ws_connections,
            ws_total_symbols, ws_status, thread_count, stuck_threads, thread_details, thread_status,
            process_details, db_sizes, overall_status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                 ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
        params![
            h.timestamp,
            h.fd_count,
            h.fd_limit,
            h.fd_usage_percent,
            match (h.fd_limit, h.fd_count) {
                (Some(l), Some(c)) => Some(l - c),
                _ => None,
            },
            h.fd_status,
            h.memory_rss_mb,
            h.memory_vms_mb,
            h.memory_percent,
            h.memory_available_mb,
            h.memory_swap_mb,
            h.memory_status,
            h.db_connections_total,
            j(&h.db_connections),
            h.db_status,
            h.ws_connections_total,
            j(&h.ws_connections),
            h.ws_total_symbols,
            h.ws_status,
            h.thread_count,
            h.stuck_threads,
            j(&h.thread_details),
            h.thread_status,
            j(&h.process_details),
            j(&h.db_sizes),
            h.overall_status,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

const HEALTH_COLS: &str =
    "id, timestamp, fd_count, fd_limit, fd_usage_percent, fd_status, memory_rss_mb,
    memory_vms_mb, memory_percent, memory_available_mb, memory_swap_mb, memory_status,
    db_connections_total, db_connections, db_status, ws_connections_total, ws_connections,
    ws_total_symbols, ws_status, thread_count, stuck_threads, thread_details, thread_status,
    process_details, db_sizes, overall_status";

fn health_from_row(r: &rusqlite::Row) -> rusqlite::Result<HealthRow> {
    let j = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Value>(&s).ok());
    Ok(HealthRow {
        id: r.get(0)?,
        timestamp: r.get(1)?,
        fd_count: r.get(2)?,
        fd_limit: r.get(3)?,
        fd_usage_percent: r.get(4)?,
        fd_status: r.get(5)?,
        memory_rss_mb: r.get(6)?,
        memory_vms_mb: r.get(7)?,
        memory_percent: r.get(8)?,
        memory_available_mb: r.get(9)?,
        memory_swap_mb: r.get(10)?,
        memory_status: r.get(11)?,
        db_connections_total: r.get(12)?,
        db_connections: j(r.get(13)?),
        db_status: r.get(14)?,
        ws_connections_total: r.get(15)?,
        ws_connections: j(r.get(16)?),
        ws_total_symbols: r.get(17)?,
        ws_status: r.get(18)?,
        thread_count: r.get(19)?,
        stuck_threads: r.get(20)?,
        thread_details: j(r.get(21)?),
        thread_status: r.get(22)?,
        process_details: j(r.get(23)?),
        db_sizes: j(r.get(24)?),
        overall_status: r.get(25)?,
    })
}

pub fn latest_health(conn: &Connection) -> Result<Option<HealthRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {HEALTH_COLS} FROM health_metrics ORDER BY id DESC LIMIT 1"),
            [],
            health_from_row,
        )
        .optional()?)
}

pub fn health_since(conn: &Connection, since: DateTime<Utc>) -> Result<Vec<HealthRow>> {
    let mut st = conn.prepare(&format!(
        "SELECT {HEALTH_COLS} FROM health_metrics WHERE timestamp >= ?1 ORDER BY id"
    ))?;
    let rows = st
        .query_map([ts(since)], health_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug, Clone, Default)]
pub struct AlertRow {
    pub id: i64,
    pub timestamp: String,
    pub alert_type: String,
    pub severity: String,
    pub metric_name: String,
    pub metric_value: f64,
    pub threshold_value: f64,
    pub message: String,
    pub acknowledged: bool,
    pub resolved: bool,
}

/// Raise an alert, or refresh the open one of the same type.
pub fn raise_alert(conn: &Connection, a: &AlertRow, now: DateTime<Utc>) -> Result<()> {
    let open: Option<i64> = conn
        .query_row(
            "SELECT id FROM health_alerts WHERE alert_type = ?1 AND resolved = 0 ORDER BY id DESC LIMIT 1",
            [&a.alert_type],
            |r| r.get(0),
        )
        .optional()?;
    match open {
        Some(id) => {
            conn.execute(
                "UPDATE health_alerts SET timestamp = ?1, metric_value = ?2 WHERE id = ?3",
                params![ts(now), a.metric_value, id],
            )?;
        }
        None => {
            conn.execute(
                "INSERT INTO health_alerts (timestamp, alert_type, severity, metric_name, metric_value,
                    threshold_value, message) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    ts(now),
                    a.alert_type,
                    a.severity,
                    a.metric_name,
                    a.metric_value,
                    a.threshold_value,
                    a.message
                ],
            )?;
        }
    }
    Ok(())
}

/// Resolve open alerts for a metric that is back under its warning level.
pub fn auto_resolve(conn: &Connection, metric: &str, now: DateTime<Utc>) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE health_alerts SET resolved = 1, resolved_at = ?1 WHERE metric_name = ?2 AND resolved = 0",
        params![ts(now), metric],
    )?)
}

pub fn active_alerts(conn: &Connection) -> Result<Vec<AlertRow>> {
    let mut st = conn.prepare(
        "SELECT id, timestamp, alert_type, severity, metric_name, metric_value, threshold_value, message,
                acknowledged, resolved
         FROM health_alerts WHERE resolved = 0 ORDER BY timestamp DESC, id DESC",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(AlertRow {
                id: r.get(0)?,
                timestamp: r.get(1)?,
                alert_type: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                severity: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                metric_name: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                metric_value: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                threshold_value: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                message: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                acknowledged: r.get(8)?,
                resolved: r.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn acknowledge_alert(conn: &Connection, id: i64, now: DateTime<Utc>) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE health_alerts SET acknowledged = 1, acknowledged_at = ?1 WHERE id = ?2",
        params![ts(now), id],
    )? > 0)
}

pub fn resolve_alert(conn: &Connection, id: i64, now: DateTime<Utc>) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE health_alerts SET resolved = 1, resolved_at = ?1 WHERE id = ?2",
        params![ts(now), id],
    )? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S-06: secrets already written by earlier builds are scrubbed once,
    /// on a populated database.
    #[test]
    fn stored_webhook_secrets_are_redacted_once() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
             applied_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )
        .unwrap();
        create_tables(&c).unwrap();
        c.execute(
            "INSERT INTO traffic_logs (timestamp, client_ip, method, path, status_code, duration_ms)
             VALUES ('t', '1.2.3.4', 'POST', '/strategy/webhook/oaws_SECRET123', 200, 1.0),
                    ('t', '1.2.3.4', 'POST', '/api/v1/placeorder', 200, 1.0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO error_404_tracker (ip_address, first_error_at, last_error_at, paths_attempted)
             VALUES ('5.6.7.8', 't', 't', '[\"/chartink/webhook/abc-SECRET\", \"/x\"]')",
            [],
        )
        .unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let paths: Vec<String> = c
            .prepare("SELECT path FROM traffic_logs ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            paths,
            vec!["/strategy/webhook/<redacted>", "/api/v1/placeorder"]
        );
        let tried: String = c
            .query_row("SELECT paths_attempted FROM error_404_tracker", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!tried.contains("SECRET"), "{}", tried);
        assert!(tried.contains("/chartink/webhook/<redacted>"));
        assert!(tried.contains("/x"));
    }

    #[test]
    fn redaction_keeps_the_route() {
        assert_eq!(
            redact_secret_paths("/strategy/webhook/oaws_abc"),
            "/strategy/webhook/<redacted>"
        );
        assert_eq!(
            redact_secret_paths("/chartink/webhook/"),
            "/chartink/webhook/"
        );
        assert_eq!(redact_secret_paths("/api/v1/funds"), "/api/v1/funds");
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
             applied_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )
        .unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        c
    }

    fn now() -> DateTime<Utc> {
        "2026-10-05T04:30:00Z".parse().unwrap()
    }

    #[test]
    fn rows_from_a_populated_main_database_are_imported_once() {
        let main = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_legacy_schema(&main).unwrap();
        main.execute(
            "INSERT INTO traffic_logs (client_ip, method, path, status_code, duration_ms)
             VALUES ('1.2.3.4', 'GET', '/x?apikey=SECRET', 200, 2.5)",
            [],
        )
        .unwrap();
        main.execute(
            "INSERT INTO ip_bans (ip_address, ban_reason, is_permanent) VALUES ('5.6.7.8', 'old', 1)",
            [],
        )
        .unwrap();
        main.execute(
            "INSERT INTO latency_logs (order_id, broker, symbol, order_type, total_ms, status)
             VALUES ('1', 'zerodha', 'SBIN', 'PLACE', 42.0, 'SUCCESS')",
            [],
        )
        .unwrap();
        let logs = db();
        import_from_main(&logs, &main).unwrap();
        import_from_main(&logs, &main).unwrap();
        let t = recent_traffic(&logs, None).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].path, "/x");
        let b = all_bans(&logs, now()).unwrap();
        assert_eq!(b.len(), 1);
        assert!(b[0].is_permanent);
        let l = recent_latency(&logs, None).unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].total_latency_ms, 42.0);
    }

    #[test]
    fn traffic_never_stores_a_query_string() {
        let c = db();
        insert_traffic(
            &c,
            &TrafficRow {
                timestamp: ts(now()),
                client_ip: "127.0.0.1".into(),
                method: "GET".into(),
                path: "/zerodha/callback?request_token=SECRET&state=x".into(),
                status_code: 302,
                duration_ms: 1.0,
                host: None,
                error: None,
            },
        )
        .unwrap();
        let r = recent_traffic(&c, Some(1)).unwrap();
        assert_eq!(r[0].path, "/zerodha/callback");
    }

    #[test]
    fn purge_bounds_every_table_by_age_and_cap() {
        let c = db();
        let old = ts(now() - Duration::days(40));
        for i in 0..20 {
            let t = if i < 10 { old.clone() } else { ts(now()) };
            c.execute(
                "INSERT INTO traffic_logs (timestamp, client_ip, method, path, status_code, duration_ms)
                 VALUES (?1, '1.2.3.4', 'GET', '/x', 200, 1.0)",
                [t],
            )
            .unwrap();
        }
        purge(&c, now()).unwrap();
        assert_eq!(count(&c, "traffic_logs").unwrap(), 10);
    }

    #[test]
    fn repeat_bans_become_permanent_and_loopback_is_never_banned() {
        let c = db();
        assert!(!ban_ip(&c, "127.0.0.1", "x", Some(1), false, "manual", now(), 2).unwrap());
        for _ in 0..3 {
            ban_ip(&c, "10.0.0.9", "x", Some(1), false, "system", now(), 2).unwrap();
        }
        let b = all_bans(&c, now()).unwrap();
        assert_eq!(b.len(), 1);
        assert!(b[0].is_permanent);
        assert_eq!(b[0].ban_count, 3);
    }

    #[test]
    fn trackers_restart_after_a_day_and_keep_50_paths() {
        let c = db();
        for i in 0..60 {
            track_404(&c, "10.0.0.1", &format!("/p{i}?q=1"), now()).unwrap();
        }
        let s = suspicious_404(&c, 1, now()).unwrap();
        assert_eq!(s[0].count, 60);
        let paths: Vec<String> = serde_json::from_str(s[0].detail.as_deref().unwrap()).unwrap();
        assert_eq!(paths.len(), 50);
        assert!(paths.iter().all(|p| !p.contains('?')));
        assert_eq!(
            track_404(&c, "10.0.0.1", "/x", now() + Duration::days(2)).unwrap(),
            1
        );
        assert_eq!(track_invalid_api_key(&c, "10.0.0.2", now()).unwrap(), 1);
        assert_eq!(track_invalid_api_key(&c, "10.0.0.2", now()).unwrap(), 2);
    }
}
