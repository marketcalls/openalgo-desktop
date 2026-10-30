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
pub fn finish(
    conn: &Connection,
    id: i64,
    status: RunStatus,
    error: Option<&str>,
    logs: &[Value],
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
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
    tracing::info!("Updated execution {} status to {}", id, status.as_str());
    Ok(())
}

/// A workflow's runs, newest first, at most `limit` clamped to
/// 1..=[`QUERY_MAX`] (`get_workflow_executions`).
pub fn list(conn: &Connection, workflow_id: i64, limit: i64) -> Result<Vec<Execution>> {
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
mod tests {
    use super::super::testing::db;
    use super::super::workflows::{create as create_workflow, NewWorkflow};
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        "2026-10-30T03:45:00Z".parse().unwrap()
    }

    fn workflow(conn: &Connection) -> i64 {
        let (_, sec) = db();
        create_workflow(
            conn,
            &sec,
            &NewWorkflow {
                name: "A",
                description: None,
                nodes: &json!([]),
                edges: &json!([]),
            },
            now(),
        )
        .unwrap()
        .id
    }

    #[test]
    fn a_run_is_recorded_running_then_finished_with_its_trace() {
        let (conn, _) = db();
        let w = workflow(&conn);
        let id = create(&conn, w, now()).unwrap();
        let running = &list(&conn, w, 20).unwrap()[0];
        assert_eq!(running.status, "running");
        assert_eq!(running.started_at.as_deref(), Some("2026-10-30T03:45:00"));
        assert_eq!(running.completed_at, None);
        let logs = vec![json!({"time": "t", "message": "Workflow started", "level": "info"})];
        finish(
            &conn,
            id,
            RunStatus::Failed,
            Some("placeOrder: rejected"),
            &logs,
            now() + Duration::seconds(2),
        )
        .unwrap();
        let done = &list(&conn, w, 20).unwrap()[0];
        assert_eq!(done.status, "failed");
        assert_eq!(done.error.as_deref(), Some("placeOrder: rejected"));
        assert_eq!(done.logs, Value::Array(logs));
        assert_eq!(done.completed_at.as_deref(), Some("2026-10-30T03:45:02"));
    }

    #[test]
    fn history_is_newest_first_and_clamped() {
        let (conn, _) = db();
        let w = workflow(&conn);
        let ids: Vec<i64> = (0..3).map(|_| create(&conn, w, now()).unwrap()).collect();
        let l = list(&conn, w, -1).unwrap();
        assert_eq!(l.len(), 1, "a negative limit is one row, never unlimited");
        assert_eq!(l[0].id, ids[2]);
        assert_eq!(list(&conn, w, 1000).unwrap().len(), 3);
    }

    #[test]
    fn history_is_trimmed_by_count_and_age() {
        let (conn, _) = db();
        let w = workflow(&conn);
        create(&conn, w, now() - Duration::days(31)).unwrap();
        for _ in 0..(RETENTION_COUNT + 5) {
            create(&conn, w, now()).unwrap();
        }
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM flow_workflow_executions WHERE workflow_id = ?1",
                params![w],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, RETENTION_COUNT);
        let oldest: String = conn
            .query_row(
                "SELECT MIN(started_at) FROM flow_workflow_executions",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(oldest, "2026-10-30 03:45:00");
    }

    #[test]
    fn runs_left_running_by_a_quit_are_closed() {
        let (conn, _) = db();
        let w = workflow(&conn);
        create(&conn, w, now()).unwrap();
        assert_eq!(close_interrupted(&conn, now()).unwrap(), 1);
        let r = &list(&conn, w, 1).unwrap()[0];
        assert_eq!(
            (r.status.as_str(), r.error.as_deref()),
            ("failed", Some(INTERRUPTED))
        );
    }

    /// X-198, X-199: the age pass alone removes a stale run among few, and
    /// a prune touches only its own workflow.
    #[test]
    fn a_stale_run_goes_by_age_and_only_from_its_workflow() {
        let (conn, _) = db();
        let w = workflow(&conn);
        let other = workflow(&conn);
        create(&conn, w, now() - Duration::days(31)).unwrap();
        let theirs = create(&conn, other, now() - Duration::days(31)).unwrap();
        let fresh = create(&conn, w, now() - Duration::days(29)).unwrap();
        assert_eq!(prune(&conn, w, now()).unwrap(), 1);
        let ids =
            |wf: i64| -> Vec<i64> { list(&conn, wf, 200).unwrap().iter().map(|e| e.id).collect() };
        assert_eq!(ids(w), vec![fresh]);
        assert_eq!(ids(other), vec![theirs]);
    }

    /// X-175: a history request reads at most QUERY_MAX rows however many
    /// it asks for, and a zero is one row.
    #[test]
    fn history_never_reads_more_than_its_cap() {
        let (conn, _) = db();
        let w = workflow(&conn);
        for _ in 0..(QUERY_MAX + 5) {
            create(&conn, w, now()).unwrap();
        }
        assert_eq!(
            list(&conn, w, 10_000_000).unwrap().len(),
            QUERY_MAX as usize
        );
        assert_eq!(list(&conn, w, 0).unwrap().len(), 1);
    }

    /// S-22, S-24, S-27: the web's `flow_db` lines for a run, and a prune
    /// that fails never fails the run.
    #[test]
    fn runs_are_logged_as_the_web_logs_them() {
        let logs = crate::services::tunnel::fake::capture_logs();
        let (conn, _) = db();
        let w = workflow(&conn);
        create(&conn, w, now() - Duration::days(40)).unwrap();
        let id = create(&conn, w, now()).unwrap();
        finish(&conn, id, RunStatus::Completed, None, &[], now()).unwrap();
        let text = logs.text();
        for line in [
            format!("Created execution for workflow {} (id={})", w, id),
            format!(
                "Pruned 1 execution(s) for workflow {} (keep newest {}, max age {}d)",
                w, RETENTION_COUNT, RETENTION_DAYS
            ),
            format!("Updated execution {} status to completed", id),
        ] {
            assert!(text.contains(&line), "{}\n{}", line, text);
        }
        conn.execute_batch(
            "CREATE TRIGGER held BEFORE DELETE ON flow_workflow_executions
             BEGIN SELECT RAISE(ABORT, 'held'); END;",
        )
        .unwrap();
        assert!(create(&conn, w, now() + Duration::days(40)).is_ok());
        assert!(
            logs.text().contains(&format!(
                "Could not prune execution history for workflow {}",
                w
            )),
            "{}",
            logs.text()
        );
    }

    /// X-199: the count pass trims only its own workflow: past the cap,
    /// one workflow keeps its newest RETENTION_COUNT and another keeps all
    /// of its runs.
    #[test]
    fn the_count_trim_keeps_another_workflows_runs() {
        let (conn, _) = db();
        let mine = workflow(&conn);
        let theirs = workflow(&conn);
        let insert = |w: i64, n: i64| {
            for _ in 0..n {
                conn.execute(
                    "INSERT INTO flow_workflow_executions (workflow_id, status, started_at, logs)
                     VALUES (?1, 'completed', ?2, '[]')",
                    params![w, stamp(now())],
                )
                .unwrap();
            }
        };
        insert(theirs, 4);
        insert(mine, RETENTION_COUNT + 3);
        insert(theirs, 2);
        assert_eq!(prune(&conn, mine, now()).unwrap(), 3);
        let count = |w: i64| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM flow_workflow_executions WHERE workflow_id = ?1",
                params![w],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!((count(mine), count(theirs)), (RETENTION_COUNT, 6));
    }
}
