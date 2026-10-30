//! The service's lines and run rules the wave-6 sweep found unpinned: the
//! web's warnings for a refused save and a blocked run (written in the
//! routes' async code, so on the test's thread), the name a blocked run
//! reads from the row, and a run holding its workflow while it is edited.

use super::*;
use crate::services::tunnel::fake::capture_logs;

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
