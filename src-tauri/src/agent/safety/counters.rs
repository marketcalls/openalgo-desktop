//! The guard's session counters in `openalgo.db` (migration
//! `099_agent_safety`): each Agent session's order count, duplicate
//! registry and pending claims.
//!
//! D-003: the web keeps these in memory (`get_guard`'s TTL cache), so a
//! restart forgot how many orders a session had claimed and which it had
//! just sent. Here a session's book is one row, read, checked and written
//! back inside one `BEGIN IMMEDIATE` transaction: the write lock taken at
//! the start is the lock the check and the claim share, across every
//! connection, so two approvals racing cannot both pass the same cap or
//! the same duplicate window.
//!
//! Same lifetime as the web's cache: a session's book lasts six hours from
//! its first check (`_GUARD_TTL_SECONDS`), after which it starts again,
//! and rows past that age are deleted, so the table stays bounded.

use super::risk::book::Book;
use crate::db::sqlite::write_transaction;
use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};

/// How long a session's book lives (the web's `_GUARD_TTL_SECONDS`).
pub const SESSION_TTL: Duration = Duration::hours(6);

/// Migration `099_agent_safety`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ag_risk_session (
            session_id TEXT PRIMARY KEY,
            book TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_ag_risk_session_created
            ON ag_risk_session (created_at);",
    )?;
    Ok(())
}

fn stamp(t: DateTime<Utc>) -> String {
    crate::agent::conversations::store::stamp(t)
}

/// Run `decide` on a session's book and keep what it changed, all in one
/// write transaction. The book a session had before is loaded first; a
/// book past [`SESSION_TTL`] starts again empty.
pub fn with_book<T>(
    conn: &Connection,
    session_id: &str,
    now: DateTime<Utc>,
    decide: impl FnOnce(&mut Book) -> T,
) -> Result<T> {
    let tx = write_transaction(conn)?;
    let cutoff = stamp(now - SESSION_TTL);
    tx.execute(
        "DELETE FROM ag_risk_session WHERE created_at <= ?1",
        [&cutoff],
    )?;
    let stored: Option<(String, String)> = tx
        .query_row(
            "SELECT book, created_at FROM ag_risk_session WHERE session_id = ?1",
            [session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (before, created_at) = match stored {
        Some((text, created)) => (parse(&text, session_id), created),
        None => (Book::default(), stamp(now)),
    };
    let mut book = before.clone();
    let out = decide(&mut book);
    if book != before {
        let text = serde_json::to_string(&book)?;
        tx.execute(
            "INSERT INTO ag_risk_session (session_id, book, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (session_id) DO UPDATE SET book = ?2, updated_at = ?4",
            params![session_id, text, created_at, stamp(now)],
        )?;
    }
    tx.commit()?;
    Ok(out)
}

/// A stored book that cannot be read fails closed for the cap: it is
/// treated as having claimed everything, so the session cap refuses
/// rather than restarting the count at zero.
fn parse(text: &str, session_id: &str) -> Book {
    serde_json::from_str(text).unwrap_or_else(|e| {
        tracing::error!(
            "The Agent risk counters of session {} could not be read; refusing new orders in it: {}",
            session_id,
            e
        );
        Book {
            orders_claimed: i64::MAX / 2,
            ..Book::default()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        c
    }

    fn claimed(n: i64) -> impl FnOnce(&mut Book) {
        move |b: &mut Book| b.orders_claimed += n
    }

    #[test]
    fn a_book_is_kept_and_read_back() {
        let c = db();
        let now = Utc::now();
        with_book(&c, "s1", now, claimed(2)).unwrap();
        with_book(&c, "s1", now, |b| {
            b.recent.insert("f".into(), now);
        })
        .unwrap();
        let b = with_book(&c, "s1", now, |b| b.clone()).unwrap();
        assert_eq!(b.orders_claimed, 2);
        assert_eq!(b.recent, BTreeMap::from([("f".to_string(), now)]));
        assert_eq!(with_book(&c, "s2", now, |b| b.orders_claimed).unwrap(), 0);
    }

    #[test]
    fn a_book_lasts_six_hours_from_its_first_check() {
        let c = db();
        let t0 = Utc::now();
        with_book(&c, "s", t0, claimed(3)).unwrap();
        with_book(&c, "s", t0 + Duration::hours(5), claimed(1)).unwrap();
        assert_eq!(
            with_book(
                &c,
                "s",
                t0 + Duration::hours(5) + Duration::minutes(59),
                |b| b.orders_claimed
            )
            .unwrap(),
            4
        );
        assert_eq!(
            with_book(&c, "s", t0 + Duration::hours(6), |b| b.orders_claimed).unwrap(),
            0
        );
        let rows: i64 = c
            .query_row("SELECT COUNT(*) FROM ag_risk_session", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "an expired book is deleted, not kept");
    }

    #[test]
    fn a_book_that_cannot_be_read_refuses_rather_than_restarting_at_zero() {
        let c = db();
        let now = Utc::now();
        c.execute(
            "INSERT INTO ag_risk_session VALUES ('s', 'not json', ?1, ?1)",
            [stamp(now)],
        )
        .unwrap();
        let count = with_book(&c, "s", now, |b| b.orders_claimed).unwrap();
        assert!(count > 1_000_000);
    }

    /// The migration on a populated database: rows written before it ran
    /// again (another table's and its own) are all still there.
    #[test]
    fn the_migration_runs_on_a_populated_database() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        let conn = t.ctx.sqlite.conn().unwrap();
        let now = Utc::now();
        crate::agent::conversations::store::create_conversation(
            &conn,
            "trader",
            Some("t"),
            "chat",
            now,
        )
        .unwrap();
        with_book(&conn, "s", now, claimed(3)).unwrap();
        conn.execute("DELETE FROM migrations WHERE name = '099_agent_safety'", [])
            .unwrap();
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        assert!(crate::db::sqlite::migrations::is_applied(&conn, "099_agent_safety").unwrap());
        assert_eq!(with_book(&conn, "s", now, |b| b.orders_claimed).unwrap(), 3);
        let conversations: i64 = conn
            .query_row("SELECT COUNT(*) FROM ag_conversation", [], |r| r.get(0))
            .unwrap();
        assert_eq!(conversations, 1);
    }
}
