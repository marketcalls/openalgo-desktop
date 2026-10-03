//! Pending broker OAuth flows.
//!
//! The server generates a random `state` when the trader starts a broker
//! login, stores only its SHA-256 with the broker and an expiry, and the
//! callback consumes it exactly once. The table never holds more than a
//! handful of rows: expired ones are purged on every insert and the count is
//! capped.

use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

pub const MAX_PENDING: i64 = 16;

pub fn hash_state(state: &str) -> String {
    hex::encode(Sha256::digest(state.as_bytes()))
}

/// A pending sign-in, consumed by its callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// The callback address the authorize URL was built with.
    pub redirect_uri: Option<String>,
}

pub fn insert(
    conn: &Connection,
    state: &str,
    broker: &str,
    redirect_uri: Option<&str>,
    now: DateTime<Utc>,
    ttl: Duration,
) -> Result<()> {
    conn.execute(
        "DELETE FROM pending_oauth WHERE expires_at <= ?1",
        [now.to_rfc3339()],
    )?;
    conn.execute(
        "INSERT INTO pending_oauth (state_hash, broker, created_at, expires_at, redirect_uri) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            hash_state(state),
            broker,
            now.to_rfc3339(),
            (now + ttl).to_rfc3339(),
            redirect_uri
        ],
    )?;
    conn.execute(
        "DELETE FROM pending_oauth WHERE state_hash NOT IN
            (SELECT state_hash FROM pending_oauth ORDER BY created_at DESC LIMIT ?1)",
        [MAX_PENDING],
    )?;
    Ok(())
}

/// Consume `state` for `broker`: the pending sign-in only for a matching,
/// unexpired, unused state. The row is deleted whether or not it was still
/// valid.
pub fn consume(
    conn: &Connection,
    state: &str,
    broker: &str,
    now: DateTime<Utc>,
) -> Result<Option<Pending>> {
    let h = hash_state(state);
    take(conn, "state_hash = ?1", &h, broker, now)
}

/// Consume the newest pending sign-in for `broker`, for a broker whose
/// redirect does not return `state` (Dhan). Only a sign-in started from
/// OpenAlgo and still unexpired matches; it is used once.
pub fn consume_latest(
    conn: &Connection,
    broker: &str,
    now: DateTime<Utc>,
) -> Result<Option<Pending>> {
    let h: Option<String> = conn
        .query_row(
            "SELECT state_hash FROM pending_oauth WHERE broker = ?1 ORDER BY created_at DESC LIMIT 1",
            [broker],
            |r| r.get(0),
        )
        .ok();
    match h {
        Some(h) => take(conn, "state_hash = ?1", &h, broker, now),
        None => Ok(None),
    }
}

fn take(
    conn: &Connection,
    filter: &str,
    key: &str,
    broker: &str,
    now: DateTime<Utc>,
) -> Result<Option<Pending>> {
    let row: Option<(String, String, Option<String>)> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT broker, expires_at, redirect_uri FROM pending_oauth WHERE {}",
            filter
        ))?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(r) => Some((r.get(0)?, r.get(1)?, r.get(2)?)),
            None => None,
        }
    };
    let Some((stored_broker, expires, redirect_uri)) = row else {
        return Ok(None);
    };
    conn.execute(
        &format!("DELETE FROM pending_oauth WHERE {}", filter),
        [key],
    )?;
    let not_expired = DateTime::parse_from_rfc3339(&expires)
        .map(|e| e.with_timezone(&Utc) > now)
        .unwrap_or(false);
    Ok((not_expired && stored_broker == broker).then_some(Pending { redirect_uri }))
}

pub fn count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM pending_oauth", [], |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A database as the previous build left it: the 040 table with a
    /// pending sign-in in flight, before migration 064.
    fn populated_old_db(now: DateTime<Utc>) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_legacy_schema(&c).unwrap();
        c.execute_batch(
            "CREATE TABLE pending_oauth (
                state_hash TEXT PRIMARY KEY,
                broker TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );",
        )
        .unwrap();
        c.execute(
            "INSERT INTO pending_oauth (state_hash, broker, created_at, expires_at) VALUES (?1, 'zerodha', ?2, ?3)",
            params![
                hash_state("old-state"),
                now.to_rfc3339(),
                (now + Duration::minutes(10)).to_rfc3339()
            ],
        )
        .unwrap();
        c
    }

    #[test]
    fn migration_keeps_a_pending_sign_in_and_records_redirects() {
        let now = Utc.with_ymd_and_hms(2026, 10, 3, 4, 0, 0).unwrap();
        let c = populated_old_db(now);
        crate::db::sqlite::migrations::run_migrations(&c).unwrap();
        // Idempotent.
        crate::db::sqlite::migrations::run_migrations(&c).unwrap();
        assert_eq!(count(&c).unwrap(), 1);
        // The in-flight sign-in still completes, with no recorded redirect.
        assert_eq!(
            consume(&c, "old-state", "zerodha", now).unwrap(),
            Some(Pending { redirect_uri: None })
        );
        assert_eq!(consume(&c, "old-state", "zerodha", now).unwrap(), None);
        // New sign-ins carry their redirect address to the code exchange.
        insert(
            &c,
            "s1",
            "upstox",
            Some("http://127.0.0.1:5500/upstox/callback"),
            now,
            Duration::minutes(10),
        )
        .unwrap();
        assert_eq!(
            consume(&c, "s1", "upstox", now).unwrap(),
            Some(Pending {
                redirect_uri: Some("http://127.0.0.1:5500/upstox/callback".into())
            })
        );
        // The master contract status table exists after the migrations.
        crate::db::sqlite::master_contract_status::update(&c, "upstox", "pending", "x", None, now)
            .unwrap();
    }

    #[test]
    fn state_less_callbacks_take_the_newest_pending_sign_in_once() {
        let now = Utc.with_ymd_and_hms(2026, 10, 3, 4, 0, 0).unwrap();
        let c = populated_old_db(now);
        crate::db::sqlite::migrations::run_migrations(&c).unwrap();
        assert_eq!(consume_latest(&c, "dhan", now).unwrap(), None);
        insert(&c, "d1", "dhan", Some("r1"), now, Duration::minutes(10)).unwrap();
        insert(
            &c,
            "d2",
            "dhan",
            Some("r2"),
            now + Duration::seconds(1),
            Duration::minutes(10),
        )
        .unwrap();
        let p = consume_latest(&c, "dhan", now + Duration::seconds(2)).unwrap();
        assert_eq!(p.unwrap().redirect_uri.as_deref(), Some("r2"));
        // Another broker's pending row is never taken.
        assert_eq!(consume_latest(&c, "fyers", now).unwrap(), None);
        // Expired rows do not match.
        let late = now + Duration::minutes(11);
        assert_eq!(consume_latest(&c, "dhan", late).unwrap(), None);
    }
}
