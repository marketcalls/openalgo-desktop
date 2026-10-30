//! The service's lines and run rules the wave-6 sweep found unpinned: the
//! web's warnings for a refused save and a blocked run (written in the
//! routes' async code, so on the test's thread), the name a blocked run
//! reads from the row, and a run holding its workflow while it is edited.

use super::*;
use crate::flow::contract::model::Execution;
use crate::services::tunnel::fake::capture_logs;
use std::time::Duration;

fn n(id: &str, t: &str, data: Value) -> Value {
    json!({"id": id, "type": t, "position": {"x": 0, "y": 0}, "data": data})
}

fn e(s: &str, t: &str) -> Value {
    json!({"id": format!("{}-{}", s, t), "source": s, "target": t})
}

fn start() -> Value {
    n("s", "start", json!({"scheduleType": "manual"}))
}

fn log(message: &str) -> Value {
    n("l", "log", json!({"message": message}))
}

const NO_TRIGGER: &str = "Workflow has no trigger node, so it can never run. Add one of: orderUpdateTrigger, priceAlert, start, webhookTrigger";

/// The captured lines that end with `text`, each checked to be a warning.
fn warnings(logs: &str, text: &str) -> usize {
    let found: Vec<&str> = logs.lines().filter(|l| l.ends_with(text)).collect();
    for l in &found {
        assert!(l.contains(" WARN "), "{}", l);
    }
    found.len()
}

/// R-15, V-130: a create the validator refuses is logged with its first
/// error, as the web's warning reads; a refused PUT logs no such line (the
/// web's update route writes none).
#[tokio::test]
async fn a_refused_save_is_logged_with_its_first_error() {
    let logs = capture_logs();
    let t = app();
    for (graph, line) in [
        (
            json!({"name": "x", "nodes": "x"}),
            "Rejected workflow save: /nodes required - Workflow needs a nodes array",
        ),
        (
            json!({"nodes": [start()], "edges": [e("s", "l")]}),
            "Rejected workflow save: /edges/0/target dangling_edge - Edge target 'l' is not a node in this workflow",
        ),
    ] {
        let r = create(&t.ctx, Body::Json(graph.clone())).await;
        assert_eq!(r.status, 400, "{} {}", graph, r.body);
        assert_eq!(warnings(&logs.text(), line), 1, "{}\n{}", line, logs.text());
    }
    let w = create_workflow(&t.ctx, "x".into(), None, json!([]), json!([]))
        .await
        .unwrap();
    let r = update(&t.ctx, w.id, Body::Json(json!({"nodes": "x"}))).await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(
        logs.text().matches("Rejected workflow save").count(),
        2,
        "{}",
        logs.text()
    );
}

/// R-33, V-134: Run Now and activation refusing a workflow that could not
/// run each log the web's warning with its first error.
#[tokio::test]
async fn a_blocked_run_is_logged_with_its_first_error() {
    let logs = capture_logs();
    let t = app();
    let w = create_workflow(&t.ctx, "Draft".into(), None, json!([log("hi")]), json!([]))
        .await
        .unwrap();
    let r = run_now(&t.ctx, w.id).await;
    assert_eq!(
        (r.status, r.body["error"].clone()),
        (400, json!("Workflow cannot be executed"))
    );
    let r = activation::activate(&t.ctx, w.id).await;
    assert_eq!(
        (r.status, r.body["error"].clone()),
        (400, json!("Workflow cannot be activated"))
    );
    let line = format!(
        "Workflow {} (Draft) blocked: /nodes no_trigger - {}",
        w.id, NO_TRIGGER
    );
    assert_eq!(
        warnings(&logs.text(), &line),
        2,
        "{}\n{}",
        line,
        logs.text()
    );
}

/// V-134 (require_name=True, the name read from the row): a finished
/// graph whose stored name is empty is refused before any run, and logged
/// with the empty name.
#[tokio::test]
async fn a_workflow_without_a_name_cannot_run() {
    let logs = capture_logs();
    let t = app();
    let w = create_workflow(
        &t.ctx,
        String::new(),
        None,
        json!([start(), log("hi")]),
        json!([e("s", "l")]),
    )
    .await
    .unwrap();
    let r = run_now(&t.ctx, w.id).await;
    assert_eq!(
        (r.status, r.body),
        (
            400,
            json!({"status": "error", "error": "Workflow cannot be executed",
                   "message": "Workflow needs a non-empty name",
                   "errors": [{"path": "/name", "code": "required",
                               "message": "Workflow needs a non-empty name",
                               "expected": "string", "received": ""}]})
        )
    );
    let line = format!(
        "Workflow {} () blocked: /name required - Workflow needs a non-empty name",
        w.id
    );
    assert_eq!(
        warnings(&logs.text(), &line),
        1,
        "{}\n{}",
        line,
        logs.text()
    );
    let runs = executions::list(&t.ctx.sqlite.conn().unwrap(), w.id, 20).unwrap();
    assert!(runs.is_empty(), "no run is recorded");
}

/// E-07: a run holds its workflow against delete and deactivate only:
/// update, replace and activate go ahead while it runs, as the web's
/// routes take no run lock.
#[tokio::test]
async fn edits_and_activation_go_ahead_while_a_run_holds_the_workflow() {
    let t = app();
    // The app's own trigger side restores active workflows at start-up on
    // its own task; stopped, only the test's hooks act.
    t.ctx.flow.triggers().stop().await;
    let hooks = Arc::new(Hooks::default());
    t.ctx.flow.set_triggers(hooks.clone());
    let edges = json!([e("s", "l")]);
    let w = create_workflow(
        &t.ctx,
        "Held".into(),
        None,
        json!([start(), log("before")]),
        edges.clone(),
    )
    .await
    .unwrap();
    let held = t.ctx.flow.try_claim(w.id).unwrap();
    let r = update(
        &t.ctx,
        w.id,
        Body::Json(json!({"nodes": [start(), log("after")]})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["nodes"][1]["data"]["message"], "after");
    let next = json!({"nodes": [start(), log("replaced")], "edges": edges});
    let r = transfer::replace(&t.ctx, w.id, Body::Json(next)).await;
    assert_eq!(
        (r.status, r.body["status"].clone()),
        (200, json!("success")),
        "{}",
        r.body
    );
    let r = activation::activate(&t.ctx, w.id).await;
    assert_eq!(
        (r.status, r.body),
        (
            200,
            json!({"status": "success", "message": "Workflow activated with start trigger"})
        )
    );
    assert_eq!(hooks.calls.lock().len(), 1, "registered once");
    let still = json!({"error": "This workflow is still running. Try again when it finishes."});
    let r = activation::deactivate(&t.ctx, w.id).await;
    assert_eq!((r.status, r.body), (409, still.clone()));
    let r = delete(&t.ctx, w.id).await;
    assert_eq!((r.status, r.body), (409, still));
    drop(held);
    let stored = load(&t.ctx, w.id).await.unwrap().unwrap();
    assert!(stored.is_active);
    assert_eq!(stored.nodes[1]["data"]["message"], "replaced");
}

/// The workflow's runs once `done` holds for them (time is paused, so each
/// wait moves it on only when nothing else can run).
async fn runs_when(
    app: &Arc<AppState>,
    id: i64,
    done: impl Fn(&[Execution]) -> bool,
) -> Vec<Execution> {
    for _ in 0..600 {
        let rows = executions::list(&app.sqlite.conn().unwrap(), id, 20).unwrap();
        if done(&rows) {
            return rows;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!(
        "the runs of workflow {} never reached the state waited for",
        id
    );
}

fn messages(run: &Execution) -> Vec<String> {
    run.logs
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["message"].as_str().map(String::from))
        .collect()
}

/// E-07: a run reads its workflow once, when it starts: an edit saved
/// while it waits in a Delay changes the next run, never this one.
#[tokio::test(start_paused = true)]
async fn an_edit_saved_during_a_run_does_not_change_that_run() {
    let t = app();
    let nodes = json!([
        start(),
        n("d", "delay", json!({"delayValue": 60})),
        log("before")
    ]);
    let w = create_workflow(
        &t.ctx,
        "Waits".into(),
        None,
        nodes,
        json!([e("s", "d"), e("d", "l")]),
    )
    .await
    .unwrap();
    let r = run_now(&t.ctx, w.id).await;
    assert_eq!(
        (r.status, r.body["accepted"].clone()),
        (202, json!(true)),
        "{}",
        r.body
    );
    // The run's record exists once it has read the workflow.
    runs_when(&t.ctx, w.id, |rows| {
        rows.first().is_some_and(|r| r.status == "running")
    })
    .await;
    let edit = workflows::Edit {
        nodes: Some(json!([start(), log("after")])),
        edges: Some(json!([e("s", "l")])),
        ..Default::default()
    };
    workflows::update(
        &t.ctx.sqlite.conn().unwrap(),
        &t.ctx.security,
        w.id,
        &edit,
        t.ctx.now(),
    )
    .unwrap()
    .unwrap();
    let rows = runs_when(&t.ctx, w.id, |rows| {
        rows.first().is_some_and(|r| r.status != "running")
    })
    .await;
    assert_eq!(rows[0].status, "completed", "{:?}", rows[0]);
    let first = messages(&rows[0]);
    assert!(
        first.contains(&"Waiting for 60 seconds".to_string()),
        "{:?}",
        first
    );
    assert!(first.contains(&"[LOG] before".to_string()), "{:?}", first);
    assert!(!first.iter().any(|m| m.contains("after")), "{:?}", first);
    let r = run_now(&t.ctx, w.id).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let rows = runs_when(&t.ctx, w.id, |rows| rows.len() == 2).await;
    let next = messages(&rows[0]);
    assert!(next.contains(&"[LOG] after".to_string()), "{:?}", next);
}
