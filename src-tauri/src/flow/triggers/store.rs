//! What the trigger side reads and writes of `flow_workflows` (migration
//! `100_flow_core`): the active flag, the scheduler job id and the next
//! scheduled run, and the active workflows' graphs for the start-up
//! restore. None of it touches the webhook secret, so it works before the
//! keys are unlocked (the app starts signed out).

use crate::error::Result;
use crate::flow::contract::model::RunStatus;
use crate::flow::store::executions;
use crate::flow::store::{iso, stamp};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// One active workflow as the restore reads it.
#[derive(Debug, Clone)]
pub struct ActiveRow {
    pub id: i64,
    pub nodes: Value,
    /// The next scheduled run the last session recorded (UTC).
    pub next_run_at: Option<DateTime<Utc>>,
}

fn parse_stamp(text: Option<String>) -> Option<DateTime<Utc>> {
    let t = text?;
    NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| n.and_utc())
}

/// Every active workflow, by id.
pub fn active(conn: &Connection) -> Result<Vec<ActiveRow>> {
    let mut st = conn.prepare(
        "SELECT id, nodes, next_run_at FROM flow_workflows WHERE is_active = 1 ORDER BY id",
    )?;
    let rows = st
        .query_map([], |r| {
            let nodes: Option<String> = r.get(1)?;
            Ok(ActiveRow {
                id: r.get(0)?,
                nodes: nodes
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or(Value::Array(Vec::new())),
                next_run_at: parse_stamp(r.get(2)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Whether a webhook token names a workflow (the unique index; nothing is
/// decrypted).
pub fn token_exists(conn: &Connection, token: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM flow_workflows WHERE webhook_token = ?1",
            params![token],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Whether the workflow exists and is active (`None`: it is gone).
pub fn is_active(conn: &Connection, id: i64) -> Result<Option<bool>> {
    Ok(conn
        .query_row(
            "SELECT is_active FROM flow_workflows WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?)
}

/// One workflow's active flag and graph (`None`: it is gone).
pub fn graph(conn: &Connection, id: i64) -> Result<Option<(bool, Value)>> {
    let row: Option<(bool, Option<String>)> = conn
        .query_row(
            "SELECT is_active, nodes FROM flow_workflows WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(active, nodes)| {
        let nodes = nodes
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Array(Vec::new()));
        (active, nodes)
    }))
}

/// Record (or clear) the scheduler job id (`set_schedule_job_id`); `false`
/// when the workflow is gone.
pub fn set_schedule_job_id(conn: &Connection, id: i64, job_id: Option<&str>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE flow_workflows SET schedule_job_id = ?1 WHERE id = ?2",
        params![job_id, id],
    )?;
    if n > 0 {
        tracing::info!(
            "Set schedule job ID {} for workflow {}",
            job_id.unwrap_or("None"),
            id
        );
    }
    Ok(n > 0)
}

/// Record (or clear) the next scheduled run (lead decision 7).
pub fn set_next_run(conn: &Connection, id: i64, next: Option<DateTime<Utc>>) -> Result<()> {
    conn.execute(
        "UPDATE flow_workflows SET next_run_at = ?1 WHERE id = ?2",
        params![next.map(stamp), id],
    )?;
    Ok(())
}

/// Stand a workflow down: inactive, no job id, no next run. `false` when
/// it is gone.
pub fn deactivate(conn: &Connection, id: i64, now: DateTime<Utc>) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE flow_workflows SET is_active = 0, schedule_job_id = NULL, next_run_at = NULL,
             updated_at = ?1 WHERE id = ?2",
        params![stamp(now), id],
    )? > 0)
}

/// A trigger that fired while it could not run (no broker session): one
/// failed run in the workflow's history saying why (lead decision 16).
pub fn record_skip(conn: &Connection, id: i64, log: Value, now: DateTime<Utc>) -> Result<()> {
    let message = log["message"].as_str().unwrap_or_default().to_string();
    let run = executions::create(conn, id, now)?;
    executions::finish(conn, run, RunStatus::Failed, Some(&message), &[log], now)
}

/// When the next run is recorded, as the web serves times (for tests and
/// the status line).
pub fn next_run(conn: &Connection, id: i64) -> Result<Option<String>> {
    let t: Option<Option<String>> = conn
        .query_row(
            "SELECT next_run_at FROM flow_workflows WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(iso(t.flatten()))
}
