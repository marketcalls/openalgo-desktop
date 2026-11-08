//! `flow_workflow_executions`: one row per run (the web's `flow_db.py`
//! execution functions). A run's row is created `running` with its start
//! time and written once more when it ends, with its whole node trace
//! (`update_execution_status(..., logs=logs)`), so a 48-node run costs two
//! writes, not fifty.

use super::{iso, stamp};
use crate::error::Result;
use crate::flow::contract::model::{Execution, RunStatus};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use serde_json::Value;

/// Runs kept per workflow, newest first (`FLOW_EXECUTION_RETENTION_COUNT`).
pub const RETENTION_COUNT: i64 = 500;
/// Days of runs kept per workflow (`FLOW_EXECUTION_RETENTION_DAYS`).
pub const RETENTION_DAYS: i64 = 30;
/// The most runs one history read returns (`EXECUTIONS_QUERY_MAX`).
pub const QUERY_MAX: i64 = 200;

/// What a run that the app's end cut off reads in its history.
pub const INTERRUPTED: &str = "OpenAlgo closed while this run was in progress; it did not finish.";

/// Record a run starting now (`create_execution(status="running")`), then
/// trim the workflow's history. The id of the new row.
pub fn create(conn: &Connection, workflow_id: i64, now: DateTime<Utc>) -> Result<i64> {
    conn.execute(
        "INSERT INTO flow_workflow_executions (workflow_id, status, started_at, logs)
         VALUES (?1, ?2, ?3, '[]')",
        params![workflow_id, RunStatus::Running.as_str(), stamp(now)],
    )?;
    let id = conn.last_insert_rowid();
    tracing::info!("Created execution for workflow {} (id={})", workflow_id, id);
    // Housekeeping: a failure here must not fail the run that caused it.
    if let Err(e) = prune(conn, workflow_id, now) {
        tracing::error!(
            error = %e,
            "Could not prune execution history for workflow {}",
            workflow_id
        );
    }
    Ok(id)
}

/// Delete runs older than [`RETENTION_DAYS`] and beyond the newest
/// [`RETENTION_COUNT`] (by id, which is never null)
/// (`prune_workflow_executions`). How many were deleted.
pub fn prune(conn: &Connection, workflow_id: i64, now: DateTime<Utc>) -> Result<usize> {
    let cutoff = stamp(now - Duration::days(RETENTION_DAYS));
    let mut deleted = conn.execute(
        "DELETE FROM flow_workflow_executions
          WHERE workflow_id = ?1 AND started_at IS NOT NULL AND started_at < ?2",
        params![workflow_id, cutoff],
    )?;
    deleted += conn.execute(
        "DELETE FROM flow_workflow_executions
          WHERE workflow_id = ?1 AND id < (
              SELECT MIN(id) FROM (
                  SELECT id FROM flow_workflow_executions WHERE workflow_id = ?1
                   ORDER BY id DESC LIMIT ?2)
          ) AND (SELECT COUNT(*) FROM flow_workflow_executions WHERE workflow_id = ?1) > ?2",
        params![workflow_id, RETENTION_COUNT],
    )?;
    if deleted > 0 {
        tracing::info!(
            "Pruned {} execution(s) for workflow {} (keep newest {}, max age {}d)",
            deleted,
            workflow_id,
            RETENTION_COUNT,
            RETENTION_DAYS
        );
    }
    Ok(deleted)
}

/// Write how a run ended, with its whole trace (`update_execution_status`).
/// The error is written only when there is one: `None` or empty keeps
/// what the row holds. A row that is gone writes nothing and logs nothing,
/// as the web returns `None` for it.
pub fn finish(
    conn: &Connection,
    id: i64,
    status: RunStatus,
    error: Option<&str>,
    logs: &[Value],
    now: DateTime<Utc>,
) -> Result<()> {
    let updated = conn.execute(
        "UPDATE flow_workflow_executions
            SET status = ?1, error = COALESCE(?2, error), logs = ?3, completed_at = ?4
          WHERE id = ?5",
        params![
            status.as_str(),
            error.filter(|e| !e.is_empty()),
            Value::Array(logs.to_vec()).to_string(),
            stamp(now),
            id
        ],
    )?;
    if updated > 0 {
        tracing::info!("Updated execution {} status to {}", id, status.as_str());
    }
    Ok(())
}

/// A workflow's runs, newest first, at most `limit` clamped to
/// 1..=[`QUERY_MAX`] (`get_workflow_executions`).
pub fn list(conn: &Connection, workflow_id: i64, limit: i64) -> Result<Vec<Execution>> {
    read(conn, workflow_id, limit).map_err(|e| {
        tracing::error!(
            "Error getting executions for workflow {}: {}",
            workflow_id,
            e
        );
        e.into()
    })
}

fn read(conn: &Connection, workflow_id: i64, limit: i64) -> rusqlite::Result<Vec<Execution>> {
    let limit = limit.clamp(1, QUERY_MAX);
    let mut st = conn.prepare(
        "SELECT id, workflow_id, status, started_at, completed_at, logs, error
           FROM flow_workflow_executions WHERE workflow_id = ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = st
        .query_map(params![workflow_id, limit], |r| {
            let logs: Option<String> = r.get(5)?;
            Ok(Execution {
                id: r.get(0)?,
                workflow_id: r.get(1)?,
                status: r.get(2)?,
                started_at: iso(r.get(3)?),
                completed_at: iso(r.get(4)?),
                logs: logs
                    .and_then(|l| serde_json::from_str(&l).ok())
                    .unwrap_or_else(|| Value::Array(Vec::new())),
                error: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Close the runs a quit or crash left `running`: they are failed with
/// [`INTERRUPTED`], so the history never shows a run as still going that
/// nothing is running. How many there were.
pub fn close_interrupted(conn: &Connection, now: DateTime<Utc>) -> Result<usize> {
    // Read first: a start with nothing to close (the usual case, and every
    // fresh folder) takes no write lock while the other stores open.
    let open: i64 = conn.query_row(
        "SELECT COUNT(*) FROM flow_workflow_executions WHERE status IN ('running', 'pending')",
        [],
        |r| r.get(0),
    )?;
    if open == 0 {
        return Ok(0);
    }
    Ok(conn.execute(
        "UPDATE flow_workflow_executions SET status = 'failed', error = ?1, completed_at = ?2
          WHERE status IN ('running', 'pending')",
        params![INTERRUPTED, stamp(now)],
    )?)
}

#[cfg(test)]
mod tests;
