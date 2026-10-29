//! A whole run against a real app context: the record, the answer, the
//! claim, the background path and the stop path.

use super::*;
use crate::flow::store::workflows::NewWorkflow;
use crate::security::Unlocked;
use std::time::Duration;

fn app() -> crate::state::testing::TestCtx {
    let t = crate::state::testing::build(
        crate::brokers::BrokerRegistry::new(),
        "2026-10-30T03:45:00Z".parse().unwrap(),
    );
    t.ctx.security.install(&Unlocked::random().unwrap());
    t
}

fn save(app: &Arc<AppState>, name: &str, nodes: Value, edges: Value) -> i64 {
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

fn key() -> RunRequest {
    RunRequest {
        webhook_data: None,
        api_key: Some(Secret::new("<APIKEY>")),
    }
}

fn history(app: &Arc<AppState>, id: i64) -> Vec<crate::flow::contract::model::Execution> {
    executions::list(&app.sqlite.conn().unwrap(), id, 20).unwrap()
}

#[tokio::test]
async fn a_run_is_answered_and_recorded_with_its_trace() {
    let t = app();
    let nodes = json!([
        node("start", "start", json!({})),
        node(
            "v",
            "variable",
            json!({"variableName": "sym", "value": "{{webhook.symbol}}"})
        ),
        node("l", "log", json!({"message": "Buying {{sym}} on {{date}}"})),
    ]);
    let id = save(
        &t.ctx,
        "Gap fade",
        nodes,
        json!([edge("start", "v", None), edge("v", "l", None)]),
    );
    let req = RunRequest {
        webhook_data: Some(json!({"symbol": "SBIN"})),
        ..key()
    };
    let out = execute(&t.ctx, id, req).await;
    assert_eq!(out["status"], "success", "{}", out);
    assert_eq!(out["message"], "Workflow executed successfully");
    let messages: Vec<&str> = out["logs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["message"].as_str())
        .collect();
    assert_eq!(messages[0], "Starting workflow: Gap fade");
    assert_eq!(messages[1], "Workflow started");
    assert_eq!(
        messages.last().copied(),
        Some("[LOG] Buying SBIN on 2026-10-30")
    );
    let row = &history(&t.ctx, id)[0];
    assert_eq!(row.id, out["execution_id"].as_i64().unwrap());
    assert_eq!(row.status, "completed");
    assert_eq!(row.logs, out["logs"]);
}

/// E-35: no key fails the run after its record exists, so the history says
/// why.
#[tokio::test]
async fn a_run_without_an_api_key_is_recorded_failed() {
    let t = app();
    let id = save(
        &t.ctx,
        "A",
        json!([node("start", "start", json!({}))]),
        json!([]),
    );
    let out = execute(&t.ctx, id, RunRequest::default()).await;
    assert_eq!(out["message"], "API key required for workflow execution");
    assert_eq!(
        out["logs"][0]["message"],
        "Error: API key required for workflow execution"
    );
    let row = &history(&t.ctx, id)[0];
    assert_eq!(
        (row.status.as_str(), row.error.as_deref()),
        ("failed", Some("API key required for workflow execution"))
    );
}

/// E-37(a): failed nodes make the run failed with the web's summary.
#[tokio::test]
async fn failed_nodes_fail_the_run_with_a_summary() {
    let t = app();
    let nodes = json!([
        node("start", "start", json!({})),
        node(
            "v",
            "variable",
            json!({"variableName": "x", "operation": "divide", "value": "0"})
        ),
    ]);
    let id = save(&t.ctx, "A", nodes, json!([edge("start", "v", None)]));
    let out = execute(&t.ctx, id, key()).await;
    let summary = "variable: Variable operation 'divide' failed: Cannot divide by zero";
    assert_eq!(out["message"], format!("1 node(s) failed: {}", summary));
    assert_eq!(out["errors"][0]["node"], "v");
    assert_eq!(history(&t.ctx, id)[0].error.as_deref(), Some(summary));
}

/// E-31, E-32: nothing is recorded for a missing or unrunnable workflow.
#[tokio::test]
async fn a_missing_or_unrunnable_workflow_records_nothing() {
    let t = app();
    assert_eq!(
        execute(&t.ctx, 999, key()).await,
        json!({"status": "error", "message": "Workflow not found"})
    );
    let id = save(&t.ctx, "A", json!("not a list"), json!([]));
    let out = execute(&t.ctx, id, key()).await;
    assert_eq!(
        out["message"],
        "Workflow cannot be executed: Workflow needs a nodes array"
    );
    assert_eq!(out["errors"][0]["path"], "/nodes");
    assert!(history(&t.ctx, id).is_empty());
}

/// E-06: a second trigger while the first runs is refused, never queued.
#[tokio::test]
async fn one_run_per_workflow_at_a_time() {
    let t = app();
    let id = save(
        &t.ctx,
        "A",
        json!([node("start", "start", json!({}))]),
        json!([]),
    );
    let held = t.ctx.flow.try_claim(id).unwrap();
    assert_eq!(
        execute(&t.ctx, id, key()).await,
        json!({"status": "error", "message": "Workflow is already running", "already_running": true})
    );
    drop(held);
    assert_eq!(execute(&t.ctx, id, key()).await["status"], "success");
    assert!(
        t.ctx.flow.try_claim(id).is_some(),
        "the claim ends with the run"
    );
}

/// Decision 2 and E-41: a run that waits more than ten seconds is answered
/// at once and goes on as an owned task; stopping the app ends its wait and
/// its record says so.
#[tokio::test]
async fn a_long_wait_answers_at_once_and_a_stop_is_recorded() {
    let t = app();
    let nodes = json!([
        node("start", "start", json!({})),
        node("d", "delay", json!({"delayValue": 60}))
    ]);
    let id = save(&t.ctx, "A", nodes, json!([edge("start", "d", None)]));
    let out = execute(&t.ctx, id, key()).await;
    assert_eq!(
        out,
        json!({"status": "accepted", "message": waits::STARTED_IN_BACKGROUND, "accepted": true})
    );
    assert!(
        t.ctx.flow.try_claim(id).is_none(),
        "the run holds the workflow"
    );
    // Wait for the run's record, not on a sleep.
    for _ in 0..200 {
        if !history(&t.ctx, id).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(history(&t.ctx, id)[0].status, "running");
    t.ctx.flow.stop_all(crate::flow::STOP_GRACE).await;
    let row = &history(&t.ctx, id)[0];
    assert_eq!(row.status, "failed");
    // The stop finds the run waiting in the Delay or, on a slow runner, just
    // before it reaches the node: either way the record says it was stopped.
    let stopped_waiting = format!(
        "delay: {}",
        crate::flow::nodes::logic::STOPPED_WHILE_WAITING
    );
    let error = row.error.clone().unwrap_or_default();
    assert!(
        error == stopped_waiting || error == super::super::walk::STOPPED,
        "{}",
        error
    );
    assert!(t.ctx.flow.try_claim(id).is_some());
}
