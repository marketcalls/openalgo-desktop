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
