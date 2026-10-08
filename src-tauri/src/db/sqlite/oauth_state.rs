//! Pending broker OAuth flows.
//!
//! The server generates a random `state` when the trader starts a broker
//! login, stores only its SHA-256 with the broker and an expiry, and the
//! callback consumes it exactly once. The table never holds more than a
//! handful of rows: expired ones are purged on every insert and the count is
//! capped.

use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

pub const MAX_PENDING: i64 = 16;

/// How long a sign-in can be completed by a redirect that does not carry
/// `state`: much shorter than the general expiry, since the browser
/// session is then the only link between the start and the callback.
pub const STATELESS_WINDOW_SECONDS: i64 = 180;

pub fn hash_state(state: &str) -> String {
    hex::encode(Sha256::digest(state.as_bytes()))
}

/// A pending sign-in, consumed by its callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// The callback address the authorize URL was built with.
    pub redirect_uri: Option<String>,
    /// Hash of the broker-issued login id recorded at the start (Dhan's
    /// `consentAppId`); a callback that repeats an id must match it.
    pub binding_hash: Option<String>,
}

impl Pending {
    /// A pending sign-in with only a redirect address.
    pub fn with_redirect(redirect_uri: Option<String>) -> Self {
        Self {
            redirect_uri,
            binding_hash: None,
        }
    }

    /// Whether `id` (from the callback) is the login id recorded at the
    /// start. True when nothing was recorded.
    pub fn binding_matches(&self, id: &str) -> bool {
        self.binding_hash
            .as_deref()
            .is_none_or(|h| h == hash_state(id))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn insert(
    conn: &Connection,
    state: &str,
    broker: &str,
    redirect_uri: Option<&str>,
    session_id: Option<&str>,
    binding: Option<&str>,
    now: DateTime<Utc>,
    ttl: Duration,
) -> Result<()> {
    conn.execute(
        "DELETE FROM pending_oauth WHERE expires_at <= ?1",
        [now.to_rfc3339()],
    )?;
    conn.execute(
        "INSERT INTO pending_oauth (state_hash, broker, created_at, expires_at, redirect_uri, session_hash, binding_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            hash_state(state),
            broker,
            now.to_rfc3339(),
            (now + ttl).to_rfc3339(),
            redirect_uri,
            session_id.map(hash_state),
            binding.map(hash_state)
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

/// Consume `state` for `broker` only if the browser session `session_id`
/// started it (an address the signed-in trader pasted). The row is
/// deleted either way.
pub fn consume_for_session(
    conn: &Connection,
    state: &str,
    broker: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<Pending>> {
    let h = hash_state(state);
    let owner: Option<Option<String>> = conn
        .query_row(
            "SELECT session_hash FROM pending_oauth WHERE state_hash = ?1",
            [&h],
            |r| r.get(0),
        )
        .optional()?;
    let p = take(conn, "state_hash = ?1", &h, broker, now)?;
    let mine = owner.flatten().is_some_and(|o| o == hash_state(session_id));
    Ok(p.filter(|_| mine))
}

/// Consume the newest pending sign-in for `broker` started from the
/// browser session `session_id`: a redirect that does not return `state`
/// (Dhan, the Noren pages; `window` is then three minutes) or an address
/// the trader pasted without one. Only the most recent sign-in of that
/// broker and browser session matches, only within `window` of its start
/// and before its expiry, and only once: every pending row of that broker
/// and session is removed whether or not one matched.
pub fn consume_latest(
    conn: &Connection,
    broker: &str,
    session_id: &str,
    now: DateTime<Utc>,
    window: Duration,
) -> Result<Option<Pending>> {
    let sh = hash_state(session_id);
    let row: Option<(String, String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT created_at, expires_at, redirect_uri, binding_hash FROM pending_oauth
             WHERE broker = ?1 AND session_hash = ?2
             ORDER BY created_at DESC LIMIT 1",
            params![broker, sh],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    conn.execute(
        "DELETE FROM pending_oauth WHERE broker = ?1 AND session_hash = ?2",
        params![broker, sh],
    )?;
    let Some((created, expires, redirect_uri, binding_hash)) = row else {
        return Ok(None);
    };
    let at = |t: &str| DateTime::parse_from_rfc3339(t).map(|d| d.with_timezone(&Utc));
    let fresh = match (at(&created), at(&expires)) {
        (Ok(c), Ok(e)) => e > now && c <= now && now - c <= window,
        _ => false,
    };
    Ok(fresh.then_some(Pending {
        redirect_uri,
        binding_hash,
    }))
}

fn take(
    conn: &Connection,
    filter: &str,
    key: &str,
    broker: &str,
    now: DateTime<Utc>,
) -> Result<Option<Pending>> {
    let row: Option<(String, String, Option<String>, Option<String>)> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT broker, expires_at, redirect_uri, binding_hash FROM pending_oauth WHERE {}",
            filter
        ))?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(r) => Some((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            None => None,
        }
    };
    let Some((stored_broker, expires, redirect_uri, binding_hash)) = row else {
        return Ok(None);
    };
    conn.execute(
        &format!("DELETE FROM pending_oauth WHERE {}", filter),
        [key],
    )?;
    let not_expired = DateTime::parse_from_rfc3339(&expires)
        .map(|e| e.with_timezone(&Utc) > now)
        .unwrap_or(false);
    Ok((not_expired && stored_broker == broker).then_some(Pending {
        redirect_uri,
        binding_hash,
    }))
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
            Some(Pending::with_redirect(None))
        );
        assert_eq!(consume(&c, "old-state", "zerodha", now).unwrap(), None);
        // New sign-ins carry their redirect address to the code exchange.
        insert(
            &c,
            "s1",
            "upstox",
            Some("http://127.0.0.1:5500/upstox/callback"),
            None,
            None,
            now,
            Duration::minutes(10),
        )
        .unwrap();
        assert_eq!(
            consume(&c, "s1", "upstox", now).unwrap(),
            Some(Pending::with_redirect(Some(
                "http://127.0.0.1:5500/upstox/callback".into()
            )))
        );
        // The master contract status table exists after the migrations.
        crate::db::sqlite::master_contract_status::update(&c, "upstox", "pending", "x", None, now)
            .unwrap();
    }

    #[test]
    fn state_less_callbacks_take_only_the_newest_fresh_sign_in_once() {
        let now = Utc.with_ymd_and_hms(2026, 10, 3, 4, 0, 0).unwrap();
        let c = populated_old_db(now);
        crate::db::sqlite::migrations::run_migrations(&c).unwrap();
        let w = Duration::seconds(STATELESS_WINDOW_SECONDS);
        assert_eq!(consume_latest(&c, "dhan", "sess", now, w).unwrap(), None);
        let ttl = Duration::minutes(10);
        let sec = Duration::seconds;
        insert(&c, "d1", "dhan", Some("r1"), Some("sess"), None, now, ttl).unwrap();
        insert(
            &c,
            "d2",
            "dhan",
            Some("r2"),
            Some("sess"),
            Some("cid"),
            now + sec(1),
            ttl,
        )
        .unwrap();
        insert(
            &c,
            "d3",
            "dhan",
            Some("r3"),
            Some("other"),
            None,
            now + sec(1),
            ttl,
        )
        .unwrap();
        let p = consume_latest(&c, "dhan", "sess", now + sec(2), w)
            .unwrap()
            .unwrap();
        assert_eq!(p.redirect_uri.as_deref(), Some("r2"));
        assert!(p.binding_matches("cid") && !p.binding_matches("someone-else"));
        // The older sign-in of the same session went with it.
        assert_eq!(
            consume_latest(&c, "dhan", "sess", now + sec(2), w).unwrap(),
            None
        );
        // Another broker's or another session's row is never taken.
        assert_eq!(consume_latest(&c, "fyers", "other", now, w).unwrap(), None);
        // The other session's row is past the three-minute window.
        let late = now + sec(STATELESS_WINDOW_SECONDS + 2);
        assert_eq!(consume_latest(&c, "dhan", "other", late, w).unwrap(), None);
        // ... and it was used up by that attempt.
        assert_eq!(
            consume_latest(&c, "dhan", "other", now + sec(2), w).unwrap(),
            None
        );
        // A pasted address completes only the sign-in its own session started.
        insert(&c, "p1", "dhan", None, Some("sess"), None, now, ttl).unwrap();
        assert_eq!(
            consume_for_session(&c, "p1", "dhan", "other", now).unwrap(),
            None
        );
        assert_eq!(consume(&c, "p1", "dhan", now).unwrap(), None, "used up");
        insert(&c, "p2", "dhan", None, Some("sess"), None, now, ttl).unwrap();
        assert!(consume_for_session(&c, "p2", "dhan", "sess", now)
            .unwrap()
            .is_some());
        // A row with state still completes by state within the general expiry.
        insert(&c, "d4", "dhan", None, Some("sess"), None, now, ttl).unwrap();
        assert!(consume(&c, "d4", "dhan", now + Duration::minutes(5))
            .unwrap()
            .is_some());
    }
}
