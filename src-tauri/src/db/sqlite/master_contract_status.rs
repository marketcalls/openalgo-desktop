//! Master contract download status per broker (web
//! `database/master_contract_status_db.py`, table `master_contract_status`,
//! same columns). One row per broker; it drives the smart download rule
//! (`services::master_contract_service`) and the `/api/master-contract/*`
//! routes.
//!
//! Times are stored as RFC 3339 UTC and shown the way the web shows them: a
//! naive ISO timestamp in IST (the web's server clock).

use crate::error::Result;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Asia::Kolkata;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A download still `downloading` after this long is reported as failed
/// (web `DOWNLOAD_TIMEOUT_MINUTES`).
pub const DOWNLOAD_TIMEOUT_MINUTES: i64 = 5;

/// Migration 066: the web's table, created when missing.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS master_contract_status (
            broker TEXT PRIMARY KEY,
            status TEXT NOT NULL DEFAULT 'pending',
            message TEXT,
            last_updated TEXT,
            total_symbols TEXT NOT NULL DEFAULT '0',
            is_ready INTEGER NOT NULL DEFAULT 0,
            last_download_time TEXT,
            download_date TEXT,
            exchange_stats TEXT,
            download_duration_seconds INTEGER
        );",
    )?;
    Ok(())
}

/// One status row.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusRow {
    pub broker: String,
    pub status: String,
    pub message: String,
    pub last_updated: Option<DateTime<Utc>>,
    pub total_symbols: String,
    pub is_ready: bool,
    pub last_download_time: Option<DateTime<Utc>>,
    pub download_date: Option<NaiveDate>,
    pub exchange_stats: Option<Value>,
    pub download_duration_seconds: Option<i64>,
}

fn ts(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// Web `datetime.isoformat()` of an IST wall-clock time.
pub fn iso_ist(t: DateTime<Utc>) -> String {
    let l = t.with_timezone(&Kolkata).naive_local();
    if l.and_utc().timestamp_subsec_micros() == 0 {
        l.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        l.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }
}

/// Mark a sign-in's download as pending (web `init_broker_status`).
pub fn init_pending(conn: &Connection, broker: &str, now: DateTime<Utc>) -> Result<()> {
    conn.execute(
        "INSERT INTO master_contract_status (broker, status, message, last_updated, is_ready)
         VALUES (?1, 'pending', 'Master contract download pending', ?2, 0)
         ON CONFLICT(broker) DO UPDATE SET status = 'pending',
            message = 'Master contract download pending', last_updated = ?2, is_ready = 0",
        params![broker, now.to_rfc3339()],
    )?;
    Ok(())
}

/// Web `update_status`: `is_ready` follows `status == "success"`.
pub fn update(
    conn: &Connection,
    broker: &str,
    status: &str,
    message: &str,
    total_symbols: Option<i64>,
    now: DateTime<Utc>,
) -> Result<()> {
    let ready = status == "success";
    conn.execute(
        "INSERT INTO master_contract_status (broker, status, message, last_updated, is_ready, total_symbols)
         VALUES (?1, ?2, ?3, ?4, ?5, COALESCE(?6, '0'))
         ON CONFLICT(broker) DO UPDATE SET status = ?2, message = ?3, last_updated = ?4,
            is_ready = ?5, total_symbols = COALESCE(?6, total_symbols)",
        params![
            broker,
            status,
            message,
            now.to_rfc3339(),
            ready,
            total_symbols.map(|n| n.to_string())
        ],
    )?;
    Ok(())
}

/// Web `update_download_stats`, after a successful download.
pub fn record_download(
    conn: &Connection,
    broker: &str,
    duration_seconds: i64,
    exchange_stats: &BTreeMap<String, i64>,
    now: DateTime<Utc>,
) -> Result<()> {
    let stats = (!exchange_stats.is_empty()).then(|| json!(exchange_stats).to_string());
    conn.execute(
        "UPDATE master_contract_status SET last_download_time = ?2, download_date = ?3,
            download_duration_seconds = ?4, exchange_stats = COALESCE(?5, exchange_stats)
         WHERE broker = ?1",
        params![
            broker,
            now.to_rfc3339(),
            now.with_timezone(&Kolkata).date_naive().to_string(),
            duration_seconds,
            stats
        ],
    )?;
    Ok(())
}

/// Web `mark_status_ready_without_download`: true when a previous download
/// exists to fall back on.
pub fn mark_ready_cached(conn: &Connection, broker: &str, now: DateTime<Utc>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE master_contract_status SET is_ready = 1, status = 'success',
            message = 'Using cached master contract', last_updated = ?2
         WHERE broker = ?1 AND last_download_time IS NOT NULL",
        params![broker, now.to_rfc3339()],
    )?;
    Ok(n > 0)
}

fn read(conn: &Connection, broker: &str) -> Result<Option<StatusRow>> {
    Ok(conn
        .query_row(
            "SELECT broker, status, message, last_updated, total_symbols, is_ready,
                last_download_time, download_date, exchange_stats, download_duration_seconds
             FROM master_contract_status WHERE broker = ?1",
            [broker],
            |r| {
                Ok(StatusRow {
                    broker: r.get(0)?,
                    status: r.get(1)?,
                    message: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    last_updated: ts(r.get(3)?),
                    total_symbols: r.get(4)?,
                    is_ready: r.get(5)?,
                    last_download_time: ts(r.get(6)?),
                    download_date: r.get::<_, Option<String>>(7)?.and_then(|d| d.parse().ok()),
                    exchange_stats: r
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    download_duration_seconds: r.get(9)?,
                })
            },
        )
        .optional()?)
}

/// Web `get_status`: a download stuck in `downloading` past the timeout is
/// turned into an error first.
pub fn get(conn: &Connection, broker: &str, now: DateTime<Utc>) -> Result<Option<StatusRow>> {
    let Some(row) = read(conn, broker)? else {
        return Ok(None);
    };
    let stuck = row.status == "downloading"
        && row
            .last_updated
            .is_some_and(|t| now - t > chrono::Duration::minutes(DOWNLOAD_TIMEOUT_MINUTES));
    if stuck {
        tracing::warn!("Master contract download for {} timed out", broker);
        update(
            conn,
            broker,
            "error",
            &format!(
                "Download timed out (stuck for >{} minutes). Click Force Download to retry.",
                DOWNLOAD_TIMEOUT_MINUTES
            ),
            None,
            now,
        )?;
        return read(conn, broker);
    }
    Ok(Some(row))
}

/// The web `get_status` dict (unknown broker: the web's placeholder).
pub fn status_json(row: Option<&StatusRow>, broker: &str) -> Value {
    match row {
        Some(s) => json!({
            "broker": s.broker,
            "status": s.status,
            "message": s.message,
            "last_updated": s.last_updated.map(iso_ist),
            "total_symbols": s.total_symbols,
            "is_ready": s.is_ready,
            "last_download_time": s.last_download_time.map(iso_ist),
            "download_date": s.download_date.map(|d| d.to_string()),
            "exchange_stats": s.exchange_stats,
            "download_duration_seconds": s.download_duration_seconds,
        }),
        None => json!({
            "broker": broker,
            "status": "unknown",
            "message": "No status available",
            "last_updated": null,
            "total_symbols": "0",
            "is_ready": false,
            "last_download_time": null,
            "download_date": null,
            "exchange_stats": null,
            "download_duration_seconds": null,
        }),
    }
}

/// When this broker last downloaded successfully.
pub fn last_download_time(conn: &Connection, broker: &str) -> Result<Option<DateTime<Utc>>> {
    Ok(read(conn, broker)?.and_then(|r| r.last_download_time))
}

/// The broker that downloaded most recently (the `symtoken` table holds one
/// broker's master at a time).
pub fn last_downloaded_broker(conn: &Connection) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT broker FROM master_contract_status WHERE last_download_time IS NOT NULL
             ORDER BY last_download_time DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        c
    }

    #[test]
    fn lifecycle_and_web_shape() {
        let c = conn();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 3, 3, 0, 0).unwrap(); // 08:30 IST
        assert_eq!(status_json(None, "zerodha")["status"], "unknown");
        init_pending(&c, "zerodha", t0).unwrap();
        update(
            &c,
            "zerodha",
            "downloading",
            "Master contract download in progress",
            None,
            t0,
        )
        .unwrap();
        let mut stats = BTreeMap::new();
        stats.insert("NSE".to_string(), 2500);
        update(
            &c,
            "zerodha",
            "success",
            "Master contract download completed successfully",
            Some(2500),
            t0,
        )
        .unwrap();
        record_download(&c, "zerodha", 12, &stats, t0).unwrap();
        let row = get(&c, "zerodha", t0).unwrap();
        let v = status_json(row.as_ref(), "zerodha");
        assert_eq!(v["status"], "success");
        assert_eq!(v["is_ready"], true);
        assert_eq!(v["total_symbols"], "2500");
        assert_eq!(v["last_download_time"], "2026-10-03T08:30:00");
        assert_eq!(v["download_date"], "2026-10-03");
        assert_eq!(v["exchange_stats"]["NSE"], 2500);
        assert_eq!(v["download_duration_seconds"], 12);
        assert_eq!(
            last_downloaded_broker(&c).unwrap().as_deref(),
            Some("zerodha")
        );
        // A new sign-in resets readiness but keeps the download history.
        init_pending(&c, "zerodha", t0).unwrap();
        assert!(mark_ready_cached(&c, "zerodha", t0).unwrap());
        let row = get(&c, "zerodha", t0).unwrap().unwrap();
        assert_eq!(row.message, "Using cached master contract");
        assert!(!mark_ready_cached(&c, "angel", t0).unwrap());
    }

    #[test]
    fn stuck_download_becomes_an_error() {
        let c = conn();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 3, 3, 0, 0).unwrap();
        update(
            &c,
            "angel",
            "downloading",
            "Master contract download in progress",
            None,
            t0,
        )
        .unwrap();
        let later = t0 + chrono::Duration::minutes(6);
        let row = get(&c, "angel", later).unwrap().unwrap();
        assert_eq!(row.status, "error");
        assert!(!row.is_ready);
        assert!(row.message.starts_with("Download timed out"));
    }

    #[test]
    fn migration_is_idempotent_and_keeps_rows() {
        let c = conn();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 3, 3, 0, 0).unwrap();
        update(&c, "fyers", "success", "done", Some(10), t0).unwrap();
        migrate(&c).unwrap();
        assert_eq!(get(&c, "fyers", t0).unwrap().unwrap().total_symbols, "10");
    }
}
