//! Run rows: create, the terminal run CAS, the live run reads and the run
//! P&L (web `strategy_module_db.py` run functions).

use super::{json_col, round_to, run_from, ts, RunRow, Store, RUN_COLS};
use crate::error::Result;
use crate::services::live_owner::LiveOwner;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;

/// Migration `090_strategy_run_account` (LOG-07): the account at the broker
/// a run was opened on, beside its broker. Checks before altering; runs
/// opened before it have none, and are compared on their broker alone.
pub fn migrate_run_account(conn: &Connection) -> Result<()> {
    if !crate::db::sqlite::migrations::column_exists(conn, "sm_strategy_run", "broker_account")? {
        conn.execute_batch(
            "ALTER TABLE sm_strategy_run ADD COLUMN broker_account TEXT NOT NULL DEFAULT ''",
        )?;
    }
    Ok(())
}

/// What recording a stop request found ([`Store::request_run_stop`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopRequest {
    /// This call recorded it.
    New,
    /// One was already recorded on the open run (a retry, or a second stop).
    Existing,
    /// Nothing is recorded: the run has ended or is not there.
    NotRecorded,
}

impl Store {
    /// A new run, with the broker session it is opened on (`owner`).
    pub fn create_run(
        &self,
        strategy_id: i64,
        mode: &str,
        owner: &LiveOwner,
        trigger_source: &str,
        webhook_event_id: Option<i64>,
        resolved_expiries: Option<&Value>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sm_strategy_run (strategy_id, mode, broker, broker_account, started_at, \
             trigger_source, webhook_event_id, resolved_expiries) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                strategy_id,
                mode,
                owner.broker,
                owner.account,
                self.utcnow(),
                trigger_source,
                webhook_event_id,
                resolved_expiries.and_then(json_col),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Record the broker session of a Live run opened before any broker
    /// was logged in (a signal run records `live` in place of a broker),
    /// once (web `bind_run_broker`, marketcalls/openalgo#2337). Conditional,
    /// so only the first caller binds and a run that has a broker keeps it:
    /// two orders racing on an unbound run cannot bind it to two sessions.
    pub fn bind_run_owner(&self, run_id: i64, owner: &LiveOwner) -> Result<bool> {
        if !owner.knows_broker() {
            return Ok(false);
        }
        let conn = self.conn()?;
        let bound = conn.execute(
            "UPDATE sm_strategy_run SET broker = ?1, broker_account = ?2 \
             WHERE id = ?3 AND mode = 'live' AND stopped_at IS NULL \
             AND LOWER(TRIM(broker)) IN ('', 'live')",
            params![owner.broker, owner.account, run_id],
        )?;
        Ok(bound == 1)
    }

    /// When the run's first order reached a broker (the broker gave it an
    /// id), or `None` if none has (web `first_dispatched_at`). A time that
    /// cannot be read is the earliest there is, so no session can be shown
    /// to have been in place before it.
    pub fn first_dispatched_at(&self, run_id: i64) -> Result<Option<DateTime<Utc>>> {
        let conn = self.conn()?;
        let first: Option<String> = conn.query_row(
            "SELECT MIN(placed_at) FROM sm_strategy_order WHERE run_id = ?1 \
             AND broker_order_id IS NOT NULL AND TRIM(broker_order_id) != ''",
            [run_id],
            |r| r.get(0),
        )?;
        Ok(first.map(|t| super::parse_utc(&t).unwrap_or(DateTime::<Utc>::MIN_UTC)))
    }

    pub fn get_run(&self, run_id: i64) -> Result<Option<RunRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM sm_strategy_run WHERE id = ?1", RUN_COLS),
                [run_id],
                run_from,
            )
            .optional()?)
    }

    pub fn list_runs(&self, strategy_id: i64, limit: i64) -> Result<Vec<Value>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_run WHERE strategy_id = ?1 \
             ORDER BY started_at DESC, id DESC LIMIT ?2",
            RUN_COLS
        ))?;
        let rows = stmt
            .query_map(params![strategy_id, limit.max(1)], run_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.iter().map(RunRow::to_dict).collect())
    }

    pub fn list_open_runs(&self) -> Result<Vec<RunRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_run WHERE stopped_at IS NULL ORDER BY id",
            RUN_COLS
        ))?;
        let rows = stmt
            .query_map([], run_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn finish_fields(&self) -> String {
        self.utcnow()
    }

    /// Finish one active run and release only its current strategy, in one
    /// transaction. False when the run was already finished or the strategy
    /// now points at another run: neither row changes then.
    pub fn finish_run_and_release_strategy(
        &self,
        run_id: i64,
        strategy_id: i64,
        stop_reason: &str,
        pnl_realized: f64,
        pnl_peak: f64,
        pnl_trough: f64,
    ) -> Result<bool> {
        let now = self.finish_fields();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let finished = tx.execute(
            "UPDATE sm_strategy_run SET stopped_at = ?1, stop_reason = ?2, stop_requested_at = NULL, \
             stop_requested_reason = NULL, pnl_realized = ?3, pnl_peak = ?4, pnl_trough = ?5 \
             WHERE id = ?6 AND strategy_id = ?7 AND stopped_at IS NULL",
            params![
                now,
                stop_reason,
                round_to(pnl_realized, 2),
                round_to(pnl_peak, 2),
                round_to(pnl_trough, 2),
                run_id,
                strategy_id
            ],
        )?;
        if finished != 1 {
            tx.rollback()?;
            return Ok(false);
        }
        let released = tx.execute(
            "UPDATE sm_strategy SET status = 'stopped', current_run_id = NULL \
             WHERE id = ?1 AND current_run_id = ?2",
            params![strategy_id, run_id],
        )?;
        if released != 1 {
            tx.rollback()?;
            return Ok(false);
        }
        tx.commit()?;
        Ok(true)
    }

    /// Finish a residual run that is not the strategy's current run.
    pub fn finish_detached_run(
        &self,
        run_id: i64,
        strategy_id: i64,
        stop_reason: &str,
        pnl_realized: f64,
        pnl_peak: f64,
        pnl_trough: f64,
    ) -> Result<bool> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "UPDATE sm_strategy_run SET stopped_at = ?1, stop_reason = ?2, stop_requested_at = NULL, \
             stop_requested_reason = NULL, pnl_realized = ?3, pnl_peak = ?4, pnl_trough = ?5 \
             WHERE id = ?6 AND strategy_id = ?7 AND stopped_at IS NULL AND NOT EXISTS \
             (SELECT 1 FROM sm_strategy WHERE id = ?7 AND current_run_id = ?6)",
            params![
                self.utcnow(),
                stop_reason,
                round_to(pnl_realized, 2),
                round_to(pnl_peak, 2),
                round_to(pnl_trough, 2),
                run_id,
                strategy_id
            ],
        )? == 1)
    }

    /// Close a never-linked run, then release only its empty strategy claim.
    pub fn finish_unlinked_run_and_release_claim(
        &self,
        run_id: i64,
        strategy_id: i64,
        stop_reason: &str,
    ) -> Result<bool> {
        if !self.finish_detached_run(run_id, strategy_id, stop_reason, 0.0, 0.0, 0.0)? {
            return Ok(false);
        }
        let conn = self.conn()?;
        Ok(conn.execute(
            "UPDATE sm_strategy SET status = 'stopped', current_run_id = NULL \
             WHERE id = ?1 AND status = 'running' AND current_run_id IS NULL",
            [strategy_id],
        )? == 1)
    }

    /// Persist a stop request while leaving the run active until it is flat,
    /// and say whether this call is the one that recorded it (web
    /// `record_run_stop_request`, marketcalls/openalgo#2337): a pending stop
    /// is asked again every few seconds and must be announced once.
    pub fn request_run_stop(&self, run_id: i64, reason: &str) -> Result<StopRequest> {
        let conn = self.conn()?;
        let updated = conn.execute(
            "UPDATE sm_strategy_run SET stop_requested_at = ?1, stop_requested_reason = ?2 \
             WHERE id = ?3 AND stopped_at IS NULL AND stop_requested_at IS NULL \
             AND stop_requested_reason IS NULL",
            params![self.utcnow(), reason, run_id],
        )?;
        if updated == 1 {
            return Ok(StopRequest::New);
        }
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM sm_strategy_run WHERE id = ?1 AND stopped_at IS NULL \
                 AND stop_requested_at IS NOT NULL AND stop_requested_reason IS NOT NULL",
                [run_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(if existing.is_some() {
            StopRequest::Existing
        } else {
            StopRequest::NotRecorded
        })
    }

    /// What earlier runs banked since `since` (a daily loss limit's session).
    pub fn realized_pnl_since(
        &self,
        strategy_id: i64,
        since: DateTime<Utc>,
        exclude_run_id: Option<i64>,
    ) -> Result<f64> {
        let conn = self.conn()?;
        let total: Option<f64> = conn.query_row(
            "SELECT SUM(pnl_realized) FROM sm_strategy_run WHERE strategy_id = ?1 \
             AND started_at >= ?2 AND id != ?3",
            params![strategy_id, ts(since), exclude_run_id.unwrap_or(-1)],
            |r| r.get(0),
        )?;
        Ok(total.unwrap_or(0.0))
    }
}
