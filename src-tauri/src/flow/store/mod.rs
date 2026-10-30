//! Flow's tables in `openalgo.db` (migration `100_flow_core`), column for
//! column as the web's `database/flow_db.py`:
//!
//! - `flow_workflows`: the graph (`nodes`, `edges` as the editor sent them),
//!   the active flag, the scheduler job id and the webhook settings (token,
//!   secret, enabled, auth type). The web keeps webhook settings on the
//!   workflow row, so no separate table (`101_flow_webhooks` is unused).
//! - `flow_workflow_executions`: one row per run, with its node trace,
//!   pruned to the newest 500 and the last 30 days per workflow.
//!
//! Departures from the web: the webhook secret is AES-GCM encrypted bound
//! to its row (`webhook_secret`, `webhook_secret_nonce`); the web's
//! `api_key` column is kept for the schema's sake but never written or
//! exported, since a run uses the trader's current API key (lead decision
//! 3); `next_run_at` holds a schedule's next due time, so a run that fell
//! due while the app was closed is judged on start (lead decision 7; the
//! scheduler writes it).
//!
//! Timestamps are UTC, stored `YYYY-MM-DD HH:MM:SS` as SQLite's
//! `CURRENT_TIMESTAMP` writes them on the web, and served as the web serves
//! them (`YYYY-MM-DDTHH:MM:SS`).

pub mod executions;
pub mod workflows;

use crate::error::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::Connection;

const STORED: &str = "%Y-%m-%d %H:%M:%S";

/// A time as stored.
pub(crate) fn stamp(now: DateTime<Utc>) -> String {
    now.format(STORED).to_string()
}

/// A stored time as the web serves it (`isoformat()` of a naive UTC time).
pub(crate) fn iso(stored: Option<String>) -> Option<String> {
    let s = stored?;
    NaiveDateTime::parse_from_str(&s, STORED)
        .ok()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Migration `100_flow_core`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS flow_workflows (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            nodes TEXT NOT NULL DEFAULT '[]',
            edges TEXT NOT NULL DEFAULT '[]',
            is_active INTEGER NOT NULL DEFAULT 0,
            schedule_job_id TEXT,
            webhook_token TEXT UNIQUE,
            webhook_secret TEXT,
            webhook_secret_nonce TEXT,
            webhook_enabled INTEGER NOT NULL DEFAULT 0,
            webhook_auth_type TEXT NOT NULL DEFAULT 'payload',
            api_key TEXT,
            next_run_at TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_flow_workflows_is_active
            ON flow_workflows (is_active);
        CREATE TABLE IF NOT EXISTS flow_workflow_executions (
            id INTEGER PRIMARY KEY,
            workflow_id INTEGER NOT NULL
                REFERENCES flow_workflows (id) ON DELETE CASCADE,
            status TEXT NOT NULL DEFAULT 'pending',
            started_at TEXT,
            completed_at TEXT,
            logs TEXT NOT NULL DEFAULT '[]',
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_flow_executions_workflow_id
            ON flow_workflow_executions (workflow_id, id);
        CREATE INDEX IF NOT EXISTS idx_flow_executions_status
            ON flow_workflow_executions (status);
        CREATE INDEX IF NOT EXISTS idx_flow_executions_started_at
            ON flow_workflow_executions (started_at);",
    )?;
    Ok(())
}

/// The keys changed (a fresh start): every webhook secret was encrypted
/// with the old key and can no longer be read, so it is dropped and the
/// webhook turned off until the trader opens its settings again, which
/// makes a new secret. A webhook never runs without one.
pub fn drop_secrets(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE flow_workflows SET webhook_secret = NULL, webhook_secret_nonce = NULL,
             webhook_enabled = 0",
        [],
    )?;
    Ok(())
}

/// The first stored webhook secret, to prove a key is the one Flow's
/// secrets were written with (the key vault's witness, after every other
/// encrypted value); `None` when no workflow holds one.
pub fn witness(conn: &Connection) -> Result<Option<crate::db::sqlite::key_vault::Witness>> {
    use rusqlite::OptionalExtension;
    let first: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT id, webhook_secret, webhook_secret_nonce FROM flow_workflows
             WHERE webhook_secret IS NOT NULL AND webhook_secret <> ''
               AND webhook_secret_nonce IS NOT NULL ORDER BY id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(first.map(
        |(id, ciphertext, nonce)| crate::db::sqlite::key_vault::Witness {
            ciphertext,
            nonce,
            aad: workflows::secret_aad(id),
        },
    ))
}

/// The account was reset: every webhook address and secret anyone holds
/// stops working, and every workflow stops (nothing may trade for an
/// account that no longer exists). History is kept.
pub fn reset_credentials(conn: &Connection) -> Result<()> {
    drop_secrets(conn)?;
    let ids: Vec<i64> = {
        let mut st = conn.prepare("SELECT id FROM flow_workflows")?;
        let v = st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        v
    };
    for id in ids {
        conn.execute(
            "UPDATE flow_workflows SET webhook_token = ?1, is_active = 0, schedule_job_id = NULL
             WHERE id = ?2",
            rusqlite::params![workflows::generate_webhook_token(), id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::security::SecurityManager;

    /// A migrated in-memory database with foreign keys on, and unlocked
    /// keys.
    pub fn db() -> (rusqlite::Connection, SecurityManager) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        super::migrate(&conn).unwrap();
        (conn, SecurityManager::for_tests())
    }
}

#[cfg(test)]
mod reads_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_migration_is_idempotent() {
        let (conn, _) = testing::db();
        migrate(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'idx_flow_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn times_are_served_as_the_web_serves_them() {
        let now = "2026-10-30T03:45:12.345Z".parse().unwrap();
        assert_eq!(stamp(now), "2026-10-30 03:45:12");
        assert_eq!(
            iso(Some(stamp(now))).as_deref(),
            Some("2026-10-30T03:45:12")
        );
        assert_eq!(iso(None), None);
    }
}
