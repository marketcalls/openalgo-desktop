//! What the approval card showed (lead decision, 2026-10-27).
//!
//! A smart order's card shows what it will send from the position held
//! when the card was drawn; the trader approves that. The order tool runs
//! after the approval, up to ten minutes later, and reads the position
//! again: if it is not the position the card showed, the order the trader
//! approved is not the order that would be checked and sent, so nothing
//! is sent. This table keeps, per paused run and order, the position the
//! card last showed (`ag_order_preview`, migration `099_agent_safety`),
//! for as long as a paused run can wait.

use crate::agent::conversations::store::stamp;
use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};

/// How long a shown position counts: as long as a paused run waits.
pub const SHOWN_FOR: Duration = Duration::minutes(10);

/// The table (part of migration `099_agent_safety`).
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ag_order_preview (
            user_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            order_key TEXT NOT NULL,
            held INTEGER NOT NULL,
            shown_at TEXT NOT NULL,
            PRIMARY KEY (user_id, run_id, order_key)
        );",
    )?;
    Ok(())
}

/// The card for `order_key` in run `run_id` showed `held` now. Older rows
/// are deleted, so the table holds only what a paused run can still use.
pub fn record(
    conn: &Connection,
    user: &str,
    run_id: &str,
    order_key: &str,
    held: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    let tx = crate::db::sqlite::write_transaction(conn)?;
    tx.execute(
        "DELETE FROM ag_order_preview WHERE shown_at <= ?1",
        [stamp(now - SHOWN_FOR)],
    )?;
    tx.execute(
        "INSERT INTO ag_order_preview (user_id, run_id, order_key, held, shown_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (user_id, run_id, order_key) DO UPDATE SET held = ?4, shown_at = ?5",
        params![user, run_id, order_key, held, stamp(now)],
    )?;
    tx.commit()?;
    Ok(())
}

/// The position the card last showed for this order in this run, when it
/// showed one within [`SHOWN_FOR`].
pub fn shown(
    conn: &Connection,
    user: &str,
    run_id: &str,
    order_key: &str,
    now: DateTime<Utc>,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT held FROM ag_order_preview
             WHERE user_id = ?1 AND run_id = ?2 AND order_key = ?3 AND shown_at > ?4",
            params![user, run_id, order_key, stamp(now - SHOWN_FOR)],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_position_shown_counts_for_ten_minutes() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        let t0 = Utc::now();
        record(&c, "trader", "run-1", "SBIN|NSE|MIS|-500|0|BUY", 500, t0).unwrap();
        assert_eq!(
            shown(&c, "trader", "run-1", "SBIN|NSE|MIS|-500|0|BUY", t0).unwrap(),
            Some(500)
        );
        record(&c, "trader", "run-1", "SBIN|NSE|MIS|-500|0|BUY", 400, t0).unwrap();
        assert_eq!(
            shown(&c, "trader", "run-1", "SBIN|NSE|MIS|-500|0|BUY", t0).unwrap(),
            Some(400)
        );
        assert_eq!(
            shown(&c, "trader", "run-2", "SBIN|NSE|MIS|-500|0|BUY", t0).unwrap(),
            None
        );
        assert_eq!(
            shown(&c, "other", "run-1", "SBIN|NSE|MIS|-500|0|BUY", t0).unwrap(),
            None
        );
        let later = t0 + SHOWN_FOR;
        assert_eq!(
            shown(&c, "trader", "run-1", "SBIN|NSE|MIS|-500|0|BUY", later).unwrap(),
            None
        );
        record(&c, "trader", "run-3", "k", 1, later).unwrap();
        let rows: i64 = c
            .query_row("SELECT COUNT(*) FROM ag_order_preview", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "rows past their time are deleted");
    }
}
