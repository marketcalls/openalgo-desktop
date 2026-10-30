//! The store's reads and no-op writes as the web's `flow_db` makes them
//! (wave 6: R-13, R-19, R-38, S-11, S-12, S-13, S-28): the newest run by
//! id, the web's log line for each failed read, an edit with nothing to
//! write, the deactivate line, and the history cap.

use super::executions::{self, RETENTION_COUNT};
use super::testing::db;
use super::workflows::{self, Edit, NewWorkflow};
use crate::flow::contract::model::RunStatus;
use crate::services::tunnel::fake::capture_logs;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use serde_json::json;

fn now() -> DateTime<Utc> {
    "2026-10-30T03:45:00Z".parse().unwrap()
}

fn workflow(conn: &Connection, sec: &crate::security::SecurityManager, name: &str) -> i64 {
    let empty = json!([]);
    workflows::create(
        conn,
        sec,
        &NewWorkflow {
            name,
            description: None,
            nodes: &empty,
            edges: &empty,
        },
        now(),
    )
    .unwrap()
    .id
}

/// R-13: the list's last run is the newest by id, whatever the start
/// times say (the web orders the history by id).
#[test]
fn the_list_shows_the_newest_run_by_id() {
    let (conn, sec) = db();
    let id = workflow(&conn, &sec, "A");
    let first = executions::create(&conn, id, now() + Duration::minutes(5)).unwrap();
    let second = executions::create(&conn, id, now()).unwrap();
    assert!(second > first);
    executions::finish(&conn, first, RunStatus::Completed, None, &[], now()).unwrap();
    executions::finish(&conn, second, RunStatus::Failed, Some("x"), &[], now()).unwrap();
    let rows = workflows::list(&conn).unwrap();
    assert_eq!(rows[0].last_execution_status.as_deref(), Some("failed"));
}

/// S-11, S-12, S-28: a read that fails is logged in the web's words and
/// reaches the caller (a 500 at the route, decision 34).
#[test]
fn failed_reads_are_logged_as_the_web_logs_them() {
    let logs = capture_logs();
    let (conn, sec) = db();
    let id = workflow(&conn, &sec, "A");
    conn.execute_batch(
        "ALTER TABLE flow_workflow_executions RENAME TO runs_gone;
         ALTER TABLE flow_workflows RENAME TO workflows_gone;",
    )
    .unwrap();
    assert!(workflows::get(&conn, &sec, id).is_err());
    assert!(workflows::list(&conn).is_err());
    assert!(executions::list(&conn, id, 20).is_err());
    let text = logs.text();
    for line in [
        format!("Error getting workflow {}: ", id),
        "Error getting all workflows: ".to_string(),
        format!("Error getting executions for workflow {}: ", id),
    ] {
        assert!(text.contains(&line), "{}\n{}", line, text);
    }
}

/// R-19: an edit with nothing to write (a PUT of non-editable keys only)
/// leaves updated_at, as the web's commit emits no UPDATE; it is logged as
/// the web's update_workflow logs it, and a missing workflow is None.
#[test]
fn an_edit_with_nothing_to_write_leaves_the_row() {
    let logs = capture_logs();
    let (conn, sec) = db();
    let id = workflow(&conn, &sec, "A");
    let before = workflows::get(&conn, &sec, id).unwrap().unwrap();
    let later = now() + Duration::hours(2);
    let after = workflows::update(&conn, &sec, id, &Edit::default(), later)
        .unwrap()
        .unwrap();
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after.updated_at.as_deref(), Some("2026-10-30T03:45:00"));
    assert!(workflows::update(&conn, &sec, 999, &Edit::default(), later)
        .unwrap()
        .is_none());
    let text = logs.text();
    assert_eq!(
        text.matches(&format!("Updated workflow {}", id)).count(),
        1,
        "{}",
        text
    );
    assert!(!text.contains("Updated workflow 999"), "{}", text);
}

/// S-13: standing a workflow down writes the web's update_workflow line,
/// and its error line when the write fails.
#[test]
fn a_stand_down_is_logged_as_the_web_logs_it() {
    let logs = capture_logs();
    let (conn, sec) = db();
    let id = workflow(&conn, &sec, "A");
    workflows::set_active(&conn, id, true, now()).unwrap();
    let before = logs.text().matches("Updated workflow").count();
    assert!(crate::flow::triggers::store::deactivate(&conn, id, now()).unwrap());
    assert_eq!(
        logs.text()
            .matches(&format!("Updated workflow {}", id))
            .count(),
        before + 1
    );
    conn.execute_batch(
        "CREATE TRIGGER frozen BEFORE UPDATE ON flow_workflows
         BEGIN SELECT RAISE(ABORT, 'held'); END;",
    )
    .unwrap();
    assert!(crate::flow::triggers::store::deactivate(&conn, id, now()).is_err());
    assert!(
        logs.text()
            .contains(&format!("Error updating workflow {}: held", id)),
        "{}",
        logs.text()
    );
}

/// R-38, S-28: a history read returns at most 200 runs, however many it
/// asks for (the web's EXECUTIONS_QUERY_MAX).
#[test]
fn a_history_read_returns_at_most_200_runs() {
    let (conn, sec) = db();
    let id = workflow(&conn, &sec, "A");
    assert!(RETENTION_COUNT > 205);
    for _ in 0..205 {
        conn.execute(
            "INSERT INTO flow_workflow_executions (workflow_id, status, started_at, logs)
             VALUES (?1, 'completed', '2026-10-30 03:45:00', '[]')",
            params![id],
        )
        .unwrap();
    }
    assert_eq!(executions::list(&conn, id, 10_000).unwrap().len(), 200);
    assert_eq!(executions::list(&conn, id, 201).unwrap().len(), 200);
    assert_eq!(executions::list(&conn, id, 199).unwrap().len(), 199);
}
