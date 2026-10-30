//! Wave 6: whole runs against a real app context, pinned where the sweep
//! found them unpinned: the answers, the stored rows and the server's log
//! lines (E-06, E-10, E-12, E-20, E-32, E-37, E-38, E-45, N-logic-05, S-24,
//! V-140, X-158, X-176, X-221).

use super::w6_walk::level_of;
use super::*;
use crate::flow::contract::model::RunStatus;
use crate::flow::store::workflows::NewWorkflow;
use crate::security::Unlocked;
use crate::services::tunnel::fake::capture_logs;
use std::time::Duration;

pub(super) const NOW: &str = "2026-10-30T03:45:00Z";

pub(super) fn app() -> crate::state::testing::TestCtx {
    let t =
        crate::state::testing::build(crate::brokers::BrokerRegistry::new(), NOW.parse().unwrap());
    t.ctx.security.install(&Unlocked::random().unwrap());
    t
}

pub(super) fn save(app: &Arc<AppState>, name: &str, nodes: Value, edges: Value) -> i64 {
    let conn = app.sqlite.conn().unwrap();
    workflows::create(
        &conn,
        &app.security,
        &NewWorkflow {
            name,
            description: None,
            nodes: &nodes,
            edges: &edges,
        },
        app.now(),
    )
    .unwrap()
    .id
}

pub(super) fn key() -> RunRequest {
    RunRequest {
        api_key: Some(Secret::new("<APIKEY>")),
        ..Default::default()
    }
}

pub(super) fn history(
    app: &Arc<AppState>,
    id: i64,
) -> Vec<crate::flow::contract::model::Execution> {
    executions::list(&app.sqlite.conn().unwrap(), id, 20).unwrap()
}

fn messages(out: &Value) -> Vec<String> {
    out["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Wait, bounded, until no run of the app is in flight.
async fn runs_end(app: &Arc<AppState>) {
    for _ in 0..2000 {
        if app.flow.running() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the run did not end");
}

/// E-06: the refusal of a second trigger is logged as the web's warning.
#[tokio::test]
async fn a_refused_trigger_is_logged_as_a_warning() {
    let t = app();
    let id = save(
        &t.ctx,
        "A",
        json!([node("start", "start", json!({}))]),
        json!([]),
    );
    let held = t.ctx.flow.try_claim(id).unwrap();
    let logs = capture_logs();
    assert_eq!(execute(&t.ctx, id, key()).await["already_running"], true);
    assert_eq!(
        level_of(&logs.text(), &format!("Workflow {} is already running", id)).as_deref(),
        Some("WARN"),
        "{}",
        logs.text()
    );
    drop(held);
}

/// E-32 / V-140: a workflow the editor saves (the draft level passes it)
/// but the strict level refuses is answered with the web's error, logged
/// at error, and never recorded.
#[tokio::test]
async fn a_workflow_refused_at_the_strict_level_is_logged_and_not_recorded() {
    let t = app();
    let nodes = json!([
        node("start", "start", json!({})),
        node("v", "variable", json!({})),
    ]);
    let edges = json!([edge("start", "v", None)]);
    let payload = json!({"name": "Gap fade", "nodes": nodes, "edges": edges});
    assert!(validator::validate_workflow(&payload, ValidateOptions::DRAFT).is_empty());
    let id = save(&t.ctx, "Gap fade", nodes, edges);
    let logs = capture_logs();
    let out = execute(&t.ctx, id, key()).await;
    let reason =
        "A variable node needs variableName. Without it the node fails at run time, not at import.";
    assert_eq!(
        out,
        json!({
            "status": "error",
            "message": format!("Workflow cannot be executed: {}", reason),
            "errors": [{"path": "/nodes/1/data/variableName", "code": "missing_required_field",
                        "message": reason, "expected": "variableName"}],
        })
    );
    let line = format!("Workflow {} (Gap fade) is not runnable: {}", id, reason);
    assert_eq!(
        level_of(&logs.text(), &line).as_deref(),
        Some("ERROR"),
        "{}",
        logs.text()
    );
    assert!(history(&t.ctx, id).is_empty());
}

/// E-37, X-158, X-176, X-221, E-20: several failed nodes are joined with
/// `; ` in the answer and the record, the record is `failed`, the answer
/// carries the record's id, and every entry of the run's log reaches the
/// app log. An inline run writes no end line of its own (E-45: only a
/// background run does).
#[tokio::test]
async fn several_failed_nodes_fail_the_run_with_a_joined_summary() {
    let t = app();
    let nodes = json!([
        node("start", "start", json!({})),
        node(
            "v1",
            "variable",
            json!({"variableName": "x", "operation": "divide", "value": "0"})
        ),
        node(
            "v2",
            "variable",
            json!({"variableName": "y", "operation": "multiply", "value": "abc"})
        ),
    ]);
    let id = save(
        &t.ctx,
        "A",
        nodes,
        json!([edge("start", "v1", None), edge("start", "v2", None)]),
    );
    let logs = capture_logs();
    let out = execute(&t.ctx, id, key()).await;
    let summary = "variable: Variable operation 'divide' failed: Cannot divide by zero; \
                   variable: Variable operation 'multiply' failed: could not convert string to float: 'abc'";
    let row = &history(&t.ctx, id)[0];
    assert_eq!(
        (row.status.as_str(), row.error.as_deref()),
        ("failed", Some(summary))
    );
    assert_eq!(out["status"], "error");
    assert_eq!(out["message"], format!("2 node(s) failed: {}", summary));
    assert_eq!(out["execution_id"], row.id);
    assert_eq!(out["logs"], row.logs);
    let failed: Vec<&Value> = out["errors"].as_array().unwrap().iter().collect();
    assert_eq!(
        (failed.len(), &failed[0]["node"], &failed[1]["node"]),
        (2, &json!("v1"), &json!("v2"))
    );
    let mut keys: Vec<&String> = out.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["errors", "execution_id", "logs", "message", "status"]
    );
    let text = logs.text();
    for entry in out["logs"].as_array().unwrap() {
        let want = if entry["level"] == "error" {
            "WARN"
        } else {
            "DEBUG"
        };
        let needle = format!("Flow: {}", entry["message"].as_str().unwrap());
        assert_eq!(
            level_of(&text, &needle).as_deref(),
            Some(want),
            "{}",
            needle
        );
    }
    assert!(!text.contains(&format!("{} ended:", id)), "{}", text);
}

/// N-logic-05, E-37: a node that raises ends the whole run: the log's last
/// entry is `Error: {e}` at level error, the record is failed with the
/// text, and the answer is the web's `{status, message, execution_id,
/// logs}`, with no `errors`.
#[tokio::test]
async fn a_node_that_raises_fails_the_run_with_an_error_entry() {
    let t = app();
    let nodes = json!([
        node("start", "start", json!({})),
        node("h", "holidays", json!({"year": [2026]})),
    ]);
    let id = save(&t.ctx, "A", nodes, json!([edge("start", "h", None)]));
    let mut out = execute(&t.ctx, id, key()).await;
    let raised =
        "int() argument must be a string, a bytes-like object or a real number, not 'list'";
    let row = &history(&t.ctx, id)[0];
    assert_eq!(
        (row.status.as_str(), row.error.as_deref()),
        ("failed", Some(raised))
    );
    let logs = out.as_object_mut().unwrap().remove("logs").unwrap();
    assert_eq!(
        out,
        json!({"status": "error", "message": raised, "execution_id": row.id})
    );
    assert_eq!(logs, row.logs);
    let entries = logs.as_array().unwrap();
    assert_eq!(
        entries.last(),
        Some(
            &json!({"time": "2026-10-30T09:15:00", "message": format!("Error: {}", raised), "level": "error"})
        )
    );
    assert_eq!(
        messages(&json!({ "logs": logs })),
        vec![
            "Starting workflow: A".to_string(),
            "Workflow started".to_string(),
            format!("Error: {}", raised),
        ]
    );
}

/// E-10, E-38: every run gets a fresh context: a variable set and a
/// condition decided by one run are not there for the next run of the
/// same workflow. A completed run's record carries no error.
#[tokio::test]
async fn every_run_starts_with_a_fresh_context() {
    let t = app();
    let nodes = json!([
        node("start", "start", json!({})),
        node("l", "log", json!({"message": "seen={{seen}}"})),
        node(
            "v",
            "variable",
            json!({"variableName": "seen", "value": "yes"})
        ),
        node(
            "c",
            "varCondition",
            json!({"leftValue": "1", "operator": "==", "rightValue": "1"})
        ),
        node("b", "log", json!({"message": "branch"})),
    ]);
    let edges = json!([
        edge("start", "l", None),
        edge("l", "v", None),
        edge("start", "c", None),
        edge("c", "b", Some("true")),
    ]);
    let id = save(&t.ctx, "A", nodes, edges);
    for run in 0..2 {
        let out = execute(&t.ctx, id, key()).await;
        assert_eq!(out["status"], "success", "{}", out);
        let logged = messages(&out);
        for line in [
            "[LOG] seen={{seen}}",
            "Var check: 1.0 == 1.0 = True",
            "[LOG] branch",
        ] {
            assert!(
                logged.iter().any(|l| l == line),
                "run {}: {:?}",
                run,
                logged
            );
        }
        assert!(!logged.iter().any(|l| l == "[LOG] seen=yes"), "run {}", run);
    }
    let rows = history(&t.ctx, id);
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(
            (row.status.as_str(), row.error.as_deref()),
            ("completed", None)
        );
    }
}

/// E-12: the run's `{{session_date}}` turns at the session expiry the
/// trader configured, not at 03:00: at 09:15 IST with a 10:00 expiry the
/// trading day is still the previous one.
#[tokio::test]
async fn a_run_turns_the_session_day_at_the_configured_expiry() {
    let t = app();
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE settings SET auto_logout_hour = 10, auto_logout_minute = 0 WHERE id = 1",
            [],
        )
        .unwrap();
    t.ctx.reload_config().unwrap();
    let nodes = json!([
        node("start", "start", json!({})),
        node("l", "log", json!({"message": "{{session_date}} {{date}}"})),
    ]);
    let id = save(&t.ctx, "A", nodes, json!([edge("start", "l", None)]));
    let out = execute(&t.ctx, id, key()).await;
    assert_eq!(
        messages(&out).last().map(String::as_str),
        Some("[LOG] 2026-10-29 2026-10-30")
    );
}

/// E-45: a background run logs its start at info and, when it fails, its
/// end as the web's `Background workflow {id} ended: {message}` at warning.
#[tokio::test]
async fn a_background_run_logs_its_start_and_its_failed_end() {
    let t = app();
    // An unreadable delay counts as a long wait, and raises when it runs.
    let nodes = json!([
        node("start", "start", json!({})),
        node("d", "delay", json!({"delayValue": "abc"})),
    ]);
    let id = save(&t.ctx, "A", nodes, json!([edge("start", "d", None)]));
    let logs = capture_logs();
    assert_eq!(execute(&t.ctx, id, key()).await["accepted"], true);
    runs_end(&t.ctx).await;
    let raised = "invalid literal for int() with base 10: 'abc'";
    assert_eq!(history(&t.ctx, id)[0].error.as_deref(), Some(raised));
    let text = logs.text();
    assert_eq!(
        level_of(
            &text,
            &format!("Workflow {} started in the background (it waits)", id)
        )
        .as_deref(),
        Some("INFO"),
        "{}",
        text
    );
    assert_eq!(
        level_of(
            &text,
            &format!("Background workflow {} ended: {}", id, raised)
        )
        .as_deref(),
        Some("WARN"),
        "{}",
        text
    );
    assert!(
        !text.contains(&format!("Workflow {} ended", id)),
        "{}",
        text
    );
}

fn store_workflow(conn: &rusqlite::Connection) -> i64 {
    let (_, sec) = crate::flow::store::testing::db();
    workflows::create(
        conn,
        &sec,
        &NewWorkflow {
            name: "A",
            description: None,
            nodes: &json!([]),
            edges: &json!([]),
        },
        NOW.parse().unwrap(),
    )
    .unwrap()
    .id
}

fn stored_error(conn: &rusqlite::Connection, w: i64, id: i64) -> Option<String> {
    executions::list(conn, w, 200)
        .unwrap()
        .into_iter()
        .find(|e| e.id == id)
        .and_then(|e| e.error)
}

/// E-38, S-24: the error is written only when there is one, as the web's
/// `if error:`: `None` or an empty text keeps what the row holds, and a
/// completed run has none.
#[test]
fn the_error_is_written_only_when_there_is_one() {
    let (conn, _) = crate::flow::store::testing::db();
    let w = store_workflow(&conn);
    let now = NOW.parse().unwrap();
    let id = executions::create(&conn, w, now).unwrap();
    executions::finish(&conn, id, RunStatus::Failed, Some("first"), &[], now).unwrap();
    for later in [None, Some("")] {
        executions::finish(&conn, id, RunStatus::Failed, later, &[], now).unwrap();
        assert_eq!(
            stored_error(&conn, w, id).as_deref(),
            Some("first"),
            "{:?}",
            later
        );
    }
    let done = executions::create(&conn, w, now).unwrap();
    executions::finish(&conn, done, RunStatus::Completed, None, &[], now).unwrap();
    assert_eq!(stored_error(&conn, w, done), None);
    let empty = executions::create(&conn, w, now).unwrap();
    executions::finish(&conn, empty, RunStatus::Failed, Some(""), &[], now).unwrap();
    assert_eq!(stored_error(&conn, w, empty), None);
}

/// S-24: finishing a run whose row is gone writes nothing and logs no
/// `Updated execution` line (the web returns `None` without a line); the
/// caller still sees no failure.
#[test]
fn finishing_a_missing_run_writes_and_logs_nothing() {
    let (conn, _) = crate::flow::store::testing::db();
    let w = store_workflow(&conn);
    let now = NOW.parse().unwrap();
    let logs = capture_logs();
    assert!(executions::finish(&conn, 4242, RunStatus::Failed, Some("x"), &[], now).is_ok());
    assert!(
        !logs.text().contains("Updated execution 4242"),
        "{}",
        logs.text()
    );
    let id = executions::create(&conn, w, now).unwrap();
    executions::finish(&conn, id, RunStatus::Completed, None, &[], now).unwrap();
    assert_eq!(
        level_of(
            &logs.text(),
            &format!("Updated execution {} status to completed", id)
        )
        .as_deref(),
        Some("INFO")
    );
}
