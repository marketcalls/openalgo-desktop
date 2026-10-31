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
//!
//! This module holds the schema, its migrations (including the one-time
//! import from openalgo.db and the scrubbing of stored secrets), the
//! retention purge and the shared helpers. Each group of tables has its own
//! file, re-exported here:
//!
//! | Module | Tables |
//! | --- | --- |
//! | `traffic` | `traffic_logs` |
//! | `bans` | `ip_bans`, manual and automatic |
//! | `trackers` | `error_404_tracker`, `invalid_api_key_tracker` |
//! | `login_activity` | `login_attempts` |
//! | `errors` | `error_logs` |
//! | `latency` | `order_latency` |
//! | `order_logs` | `order_logs` (the Logs page reads) |
//! | `health` | `health_metrics`, `health_alerts` |

use crate::error::Result;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

mod bans;
mod errors;
mod health;
mod latency;
mod login_activity;
mod order_logs;
mod trackers;
mod traffic;

pub use bans::*;
pub use errors::*;
pub use health::*;
pub use latency::*;
pub use login_activity::*;
pub use order_logs::*;
pub use trackers::*;
pub use traffic::*;

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
/// token, the Chartink webhook id, the Flow webhook token; a Flow symbol
/// segment after it is kept).
const SECRET_PATH_PREFIXES: [&str; 3] =
    ["/strategy/webhook/", "/chartink/webhook/", "/flow/webhook/"];
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
                     OR {c} LIKE '%/chartink/webhook/%' OR {c} LIKE '%/flow/webhook/%'",
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

/// Migration `012_canonical_ban_addresses` of `logs.db`: every stored ban in
/// the one spelling the checks look up (`::ffff:1.2.3.4` is `1.2.3.4`, IPv6
/// compressed and lower case), so a ban written another way by an earlier
/// build is not skipped. Two rows for one address become one, keeping the
/// stricter ban. Idempotent and recorded once.
fn canonical_ban_addresses(conn: &Connection) -> Result<()> {
    if applied(conn, "012_canonical_ban_addresses")? {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let r = (|| -> Result<()> {
        let rows: Vec<(i64, String, bool, Option<String>)> = {
            let mut st = conn.prepare(
                "SELECT id, ip_address, is_permanent, expires_at FROM ip_bans ORDER BY id",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            v
        };
        for (id, ip, permanent, expires) in rows {
            let Some(canonical) = crate::server::addr::canonical_text(&ip) else {
                continue;
            };
            if canonical == ip {
                continue;
            }
            let keep: Option<i64> = conn
                .query_row(
                    "SELECT id FROM ip_bans WHERE ip_address = ?1",
                    [&canonical],
                    |r| r.get(0),
                )
                .optional()?;
            match keep {
                Some(keep) => {
                    conn.execute(
                        "UPDATE ip_bans SET
                             is_permanent = MAX(is_permanent, ?2),
                             expires_at = CASE
                                 WHEN MAX(is_permanent, ?2) = 1 THEN NULL
                                 WHEN ?3 IS NOT NULL AND (expires_at IS NULL OR ?3 > expires_at) THEN ?3
                                 ELSE expires_at END
                         WHERE id = ?1",
                        params![keep, permanent, expires],
                    )?;
                    conn.execute("DELETE FROM ip_bans WHERE id = ?1", [id])?;
                }
                None => {
                    conn.execute(
                        "UPDATE ip_bans SET ip_address = ?1 WHERE id = ?2",
                        params![canonical, id],
                    )?;
                }
            }
        }
        conn.execute(
            "INSERT OR IGNORE INTO migrations (name) VALUES ('012_canonical_ban_addresses')",
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

/// Migration `013_alert_severity_warn` of `logs.db`: the ban alerts that
/// earlier builds raised with severity "warning" become "warn", the web's
/// level and the one the Health page colours (they showed no colour). An
/// open one would keep "warning" for good, since raising it again updates
/// the open row. Recorded once.
fn warn_severity(conn: &Connection) -> Result<()> {
    if applied(conn, "013_alert_severity_warn")? {
        return Ok(());
    }
    let tx = super::write_transaction(conn)?;
    tx.execute(
        "UPDATE health_alerts SET severity = 'warn' WHERE severity = 'warning'",
        [],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO migrations (name) VALUES ('013_alert_severity_warn')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Schema for the monitoring tables (idempotent). Runs on every open.
pub fn migrate(conn: &Connection) -> Result<()> {
    create_tables(conn)?;
    redact_stored_paths(conn)?;
    canonical_ban_addresses(conn)?;
    warn_severity(conn)
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
        import_traffic(logs, main)?;
        import_bans(logs, main)?;
        import_latency(logs, main)?;
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

/// The request log, newest 100 000 rows, oldest first (queries stripped).
fn import_traffic(logs: &Connection, main: &Connection) -> Result<()> {
    if !main_has_table(main, "traffic_logs")? {
        return Ok(());
    }
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
    Ok(())
}

/// The banned addresses.
fn import_bans(logs: &Connection, main: &Connection) -> Result<()> {
    if !main_has_table(main, "ip_bans")? {
        return Ok(());
    }
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
    Ok(())
}

/// The order latency log, newest 200 000 rows, oldest first.
fn import_latency(logs: &Connection, main: &Connection) -> Result<()> {
    if !main_has_table(main, "latency_logs")? {
        return Ok(());
    }
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
    Ok(())
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
                    ('t', '1.2.3.4', 'POST', '/api/v1/placeorder', 200, 1.0),
                    ('t', '1.2.3.4', 'POST', '/flow/webhook/tokFLOWSECRET/SBIN', 200, 1.0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO error_404_tracker (ip_address, first_error_at, last_error_at, paths_attempted)
             VALUES ('5.6.7.8', 't', 't', '[\"/chartink/webhook/abc-SECRET\", \"/x\", \"/flow/webhook/tokSECRET/NIFTY\"]'),
                    ('5.6.7.9', 't', 't', '[\"/flow/webhook/onlyFLOWSECRET\"]')",
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
        // T-108: rows whose only secret path is a Flow one are scrubbed too
        // (the Flow clause of the LIKE, not a Chartink path beside it).
        assert_eq!(
            paths,
            vec![
                "/strategy/webhook/<redacted>",
                "/api/v1/placeorder",
                "/flow/webhook/<redacted>/SBIN"
            ]
        );
        let flow_only: String = c
            .query_row(
                "SELECT paths_attempted FROM error_404_tracker WHERE ip_address = '5.6.7.9'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flow_only, "[\"/flow/webhook/<redacted>\"]");
        let tried: String = c
            .query_row(
                "SELECT paths_attempted FROM error_404_tracker WHERE ip_address = '5.6.7.8'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!tried.contains("SECRET"), "{}", tried);
        assert!(tried.contains("/chartink/webhook/<redacted>"));
        assert!(
            tried.contains("/flow/webhook/<redacted>/NIFTY"),
            "{}",
            tried
        );
        assert!(tried.contains("/x"));
    }

    /// Bans stored in other spellings by earlier builds become one row per
    /// address in the canonical spelling, keeping the stricter ban.
    /// Automatic bans: network devices only (never loopback, the tunnel
    /// identity or exactly one of this machine's own addresses), keyed as
    /// counted, always expiring, never shortening a ban in force, and
    /// capped. Two devices in the machine's own IPv6 prefix are banned one
    /// by one.
    #[test]
    fn automatic_bans_apply_to_network_devices_only() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
             applied_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )
        .unwrap();
        create_tables(&c).unwrap();
        let now = Utc::now();
        let own: Vec<std::net::IpAddr> = vec!["2001:db8:aa:bb::5".parse().unwrap()];
        let ban = |ip: &str, cap: i64| auto_ban(&c, ip, "test", None, now, &own, cap).unwrap();
        for ip in ["127.0.0.1", "::1", "100::2", "100::1", "2001:db8:aa:bb::5"] {
            for _ in 0..50 {
                assert!(!ban(ip, AUTO_BAN_CAP), "{} was banned", ip);
            }
        }
        assert!(ban("192.168.1.66", AUTO_BAN_CAP));
        assert!(ban("2001:db8:cc:dd::", AUTO_BAN_CAP));
        // A device in our own prefix is banned by its own address; its
        // neighbour is not.
        assert!(ban("2001:db8:aa:bb::6", AUTO_BAN_CAP));
        let banned = |ip: &str| -> i64 {
            c.query_row(
                "SELECT COUNT(*) FROM ip_bans WHERE ip_address = ?1",
                [ip],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(banned("2001:db8:aa:bb::6"), 1);
        assert_eq!(banned("2001:db8:aa:bb::7"), 0);
        assert_eq!(banned("2001:db8:aa:bb::"), 0);
        // Repeated: still expiring, never permanent.
        for _ in 0..10 {
            c.execute(
                "UPDATE ip_bans SET expires_at = ?1 WHERE ip_address = '192.168.1.66'",
                [ts(now - chrono::Duration::hours(1))],
            )
            .unwrap();
            assert!(ban("192.168.1.66", AUTO_BAN_CAP));
        }
        let permanent: bool = c
            .query_row(
                "SELECT is_permanent FROM ip_bans WHERE ip_address = '192.168.1.66'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!permanent);
        // A ban entered by hand is never shortened.
        ban_ip(
            &c,
            Ban {
                ip: "192.168.1.70",
                reason: "manual",
                duration_hours: None,
                permanent: true,
                created_by: "admin",
            },
            now,
            5,
        )
        .unwrap();
        assert!(!ban("192.168.1.70", AUTO_BAN_CAP));
        let still: bool = c
            .query_row(
                "SELECT is_permanent FROM ip_bans WHERE ip_address = '192.168.1.70'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(still);
        // The cap: three automatic bans are in force. A full list keeps
        // every ban in force, adds none, and raises an alert.
        let in_force = || -> i64 {
            c.query_row(
                "SELECT COUNT(*) FROM ip_bans WHERE created_by = 'system'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(in_force(), 3);
        assert!(!ban("192.168.1.80", 3));
        assert_eq!(in_force(), 3, "no ban was lifted to make room");
        // "warn", the severity the Health page colours (it said "warning").
        assert!(active_alerts(&c)
            .unwrap()
            .iter()
            .any(|a| a.alert_type == "ban_list_full" && a.severity == "warn"));
        assert!(ban("192.168.1.80", 4));
    }

    /// Our own IPv6 network is never banned as a whole: when many of its
    /// devices are banned one by one, the trader gets a health alert.
    #[test]
    fn many_banned_devices_in_our_network_raise_an_alert() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
             applied_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )
        .unwrap();
        create_tables(&c).unwrap();
        let now = Utc::now();
        let own: Vec<std::net::IpAddr> = vec!["2001:db8:aa:bb::5".parse().unwrap()];
        let alerts = || {
            active_alerts(&c)
                .unwrap()
                .into_iter()
                .filter(|a| a.alert_type == "banned_devices_on_network")
                .count()
        };
        for n in 1..BANNED_DEVICES_ALERT {
            let ip = format!("2001:db8:aa:bb::{:x}", 0x100 + n);
            assert!(auto_ban(&c, &ip, "test", None, now, &own, AUTO_BAN_CAP).unwrap());
        }
        assert_eq!(alerts(), 0);
        assert!(auto_ban(
            &c,
            "2001:db8:aa:bb::ff",
            "test",
            None,
            now,
            &own,
            AUTO_BAN_CAP
        )
        .unwrap());
        assert_eq!(alerts(), 1);
        let severity = active_alerts(&c)
            .unwrap()
            .into_iter()
            .find(|a| a.alert_type == "banned_devices_on_network")
            .map(|a| a.severity);
        assert_eq!(severity.as_deref(), Some("warn"));
        let whole: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM ip_bans WHERE ip_address = '2001:db8:aa:bb::'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(whole, 0, "the network itself is never banned");
    }

    #[test]
    fn stored_bans_are_rewritten_in_one_spelling() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
             applied_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )
        .unwrap();
        create_tables(&c).unwrap();
        c.execute_batch(
            "INSERT INTO ip_bans (ip_address, banned_at, expires_at, is_permanent)
                 VALUES ('1.2.3.4', 't', '2026-01-01T00:00:00Z', 0),
                        ('::ffff:1.2.3.4', 't', NULL, 1),
                        ('2001:DB8:0:0::1', 't', '2026-01-01T00:00:00Z', 0);",
        )
        .unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let rows: Vec<(String, bool)> = c
            .prepare("SELECT ip_address, is_permanent FROM ip_bans ORDER BY ip_address")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("1.2.3.4".to_string(), true),
                ("2001:db8::1".to_string(), false)
            ]
        );
        // Unbanning by any spelling finds the row.
        assert!(unban_ip(&c, "[2001:DB8::1]").unwrap());
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
        assert_eq!(
            redact_secret_paths("/flow/webhook/abcDEF_-123/SBIN"),
            "/flow/webhook/<redacted>/SBIN"
        );
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
        assert!(!ban_ip(
            &c,
            Ban {
                ip: "127.0.0.1",
                reason: "x",
                duration_hours: Some(1),
                permanent: false,
                created_by: "manual"
            },
            now(),
            2
        )
        .unwrap());
        for _ in 0..3 {
            ban_ip(
                &c,
                Ban {
                    ip: "10.0.0.9",
                    reason: "x",
                    duration_hours: Some(1),
                    permanent: false,
                    created_by: "system",
                },
                now(),
                2,
            )
            .unwrap();
        }
        let b = all_bans(&c, now()).unwrap();
        assert_eq!(b.len(), 1);
        assert!(b[0].is_permanent);
        assert_eq!(b[0].ban_count, 3);
    }

    /// The Repeat Limit is the number of bans before the permanent one, as
    /// the Security page says (the web makes the limit-th ban permanent),
    /// and an automatic ban is never made permanent by it (docs-47).
    #[test]
    fn the_repeat_limit_counts_the_bans_before_the_permanent_one() {
        let c = db();
        let by_hand = Ban {
            ip: "10.0.0.7",
            reason: "x",
            duration_hours: Some(1),
            permanent: false,
            created_by: "manual",
        };
        let permanent = |c: &Connection| all_bans(c, now()).unwrap()[0].is_permanent;
        assert!(ban_ip(&c, by_hand, now(), 2).unwrap());
        assert!(ban_ip(&c, by_hand, now(), 2).unwrap());
        assert!(!permanent(&c), "the second of a limit of 2 still expires");
        assert!(ban_ip(&c, by_hand, now(), 2).unwrap());
        assert!(permanent(&c), "the third is permanent");

        let c = db();
        for hour in 0..5 {
            let later = now() + Duration::hours(2 * hour);
            assert!(auto_ban(&c, "10.0.0.8", "x", Some(1), later, &[], 10).unwrap());
        }
        let b = all_bans(&c, now() + Duration::hours(8)).unwrap();
        assert_eq!(b[0].ban_count, 5);
        assert!(!b[0].is_permanent, "an automatic ban always expires");
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

    fn logs_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
             applied_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )
        .unwrap();
        create_tables(&c).unwrap();
        c
    }

    /// Ban alerts that earlier builds raised as "warning" become "warn",
    /// once, on a populated database; others keep theirs.
    #[test]
    fn alerts_stored_as_warning_become_warn() {
        let c = logs_db();
        c.execute(
            "INSERT INTO health_alerts (timestamp, alert_type, severity, message)
             VALUES ('t', 'ban_list_full', 'warning', 'm'), ('t', 'fd_count_fail', 'fail', 'm')",
            [],
        )
        .unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let severities: Vec<String> = c
            .prepare("SELECT severity FROM health_alerts ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(severities, ["warn", "fail"]);
    }

    /// An open alert raised again shows what is wrong now: its message and
    /// level, not the first one's (a count, a broker, a cause).
    #[test]
    fn an_open_alert_raised_again_says_what_is_wrong_now() {
        let c = logs_db();
        let raise = |value: f64, message: &str| {
            raise_alert(
                &c,
                &AlertRow {
                    alert_type: "strategy_ticks_missed".into(),
                    severity: "warn".into(),
                    metric_name: "strategy_ticks_missed".into(),
                    metric_value: value,
                    threshold_value: value - 1.0,
                    message: message.into(),
                    ..Default::default()
                },
                Utc::now(),
            )
            .unwrap()
        };
        raise(3.0, "missed 3 prices");
        raise(9.0, "missed 9 prices");
        let open = active_alerts(&c).unwrap();
        assert_eq!(open.len(), 1, "one open alert per type");
        assert_eq!(open[0].message, "missed 9 prices");
        assert_eq!((open[0].metric_value, open[0].threshold_value), (9.0, 8.0));
    }
}
