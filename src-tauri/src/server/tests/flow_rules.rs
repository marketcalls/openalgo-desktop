//! Flow's route rules the audit found unpinned (wave 3): the exact route
//! table, import and replace refusals and migrations, the body limit, run
//! now's answers, and what never consults the validator.

use super::*;

struct G {
    h: H,
    cookie: String,
    csrf: String,
}

impl G {
    fn new() -> Self {
        let h = H::new();
        h.setup();
        let (cookie, csrf) = h.session(true);
        G { h, cookie, csrf }
    }

    async fn call(&self, method: Method, path: &str, body: Option<&str>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ACCEPT, "application/json");
        if body.is_some() {
            b = b.header(header::CONTENT_TYPE, "application/json");
        }
        let req = b.body(Body::from(body.unwrap_or("").to_string())).unwrap();
        self.h
            .json(with_session(req, &self.cookie, Some(&self.csrf)))
            .await
    }

    async fn post(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        self.call(Method::POST, path, Some(&body.to_string())).await
    }

    async fn create(&self, body: Value) -> i64 {
        let (s, v) = self.post("/flow/api/workflows", &body).await;
        assert_eq!(s, StatusCode::CREATED, "{}", v);
        v["id"].as_i64().unwrap()
    }
}

fn n(id: &str, t: &str, data: Value) -> Value {
    json!({"id": id, "type": t, "position": {"x": 0, "y": 0}, "data": data})
}

fn e(s: &str, t: &str) -> Value {
    json!({"id": format!("{}-{}", s, t), "source": s, "target": t})
}

fn runnable(name: &str) -> Value {
    json!({"name": name, "nodes": [n("s", "start", json!({"scheduleType": "manual"})),
                                   n("l", "log", json!({"message": "hello"}))],
           "edges": [e("s", "l")]})
}

/// A workflow whose position check is written the old way: valid only once
/// its operator is migrated to a condition.
fn legacy(name: &str) -> Value {
    json!({"name": name, "nodes": [n("s", "start", json!({"scheduleType": "manual"})),
        n("pc", "positionCheck", json!({"symbol": "SBIN", "exchange": "NSE", "operator": ">", "threshold": 10}))],
        "edges": [e("s", "pc")]})
}

const LEGACY_NOTE: &str =
    "positionCheck 'pc': operator '>' -> condition 'quantity_above' (threshold kept)";

/// R-01: the Flow route table is exactly the web's 24 routes, method for
/// method (an extra session route fails here).
#[tokio::test]
async fn the_flow_routes_are_exactly_the_webs() {
    let mut web = vec![
        "GET /flow/api/workflows",
        "POST /flow/api/workflows",
        "GET /flow/api/workflows/{id}",
        "PUT /flow/api/workflows/{id}",
        "DELETE /flow/api/workflows/{id}",
        "POST /flow/api/workflows/{id}/activate",
        "POST /flow/api/workflows/{id}/deactivate",
        "POST /flow/api/workflows/{id}/execute",
        "GET /flow/api/workflows/{id}/executions",
        "GET /flow/api/workflows/{id}/webhook",
        "POST /flow/api/workflows/{id}/webhook/enable",
        "POST /flow/api/workflows/{id}/webhook/disable",
        "POST /flow/api/workflows/{id}/webhook/regenerate",
        "POST /flow/api/workflows/{id}/webhook/regenerate-secret",
        "POST /flow/api/workflows/{id}/webhook/auth-type",
        "POST /flow/webhook/{token}",
        "POST /flow/webhook/{token}/{symbol}",
        "GET /flow/api/monitor/status",
        "GET /flow/api/workflows/{id}/export",
        "POST /flow/api/workflows/import",
        "POST /flow/api/workflows/{id}/replace",
        "GET /flow/api/index-symbols",
        "POST /flow/api/symbol-lotsizes",
        "GET /flow/api/option-strikes",
    ];
    let mut ours: Vec<String> = crate::server::routes::flow::table()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    web.sort_unstable();
    ours.sort_unstable();
    assert_eq!(ours, web);
}

/// IE-01: exporting a workflow that does not exist is the web's 404.
#[tokio::test]
async fn exporting_a_missing_workflow_is_not_found() {
    let g = G::new();
    let (s, v) = g
        .call(Method::GET, "/flow/api/workflows/9999/export", None)
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"error": "Workflow not found"})
        )
    );
}

/// IE-06, IE-07, V-137, X-313: import migrates first, then validates as a
/// finished workflow; a refusal carries the validator's errors, and a
/// migrated import says what it changed.
#[tokio::test]
async fn import_migrates_then_validates() {
    let g = G::new();
    let (s, v) = g.post("/flow/api/workflows/import", &legacy("Old")).await;
    assert_eq!(s, StatusCode::CREATED, "{}", v);
    assert_eq!(v["migrations"], json!([LEGACY_NOTE]));
    let id = v["workflow_id"].as_i64().unwrap();
    let (_, one) = g
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(one["name"], "Old (imported)");
    assert_eq!(one["nodes"][1]["data"]["condition"], "quantity_above");
    let mut bad = runnable("Bad");
    bad["nodes"][1]["type"] = json!("Nope");
    let (s, v) = g.post("/flow/api/workflows/import", &bad).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(v["status"], "error");
    assert_eq!(v["error"], "Invalid workflow format");
    assert_eq!(v["message"], v["errors"][0]["message"]);
    assert_eq!(v["errors"][0]["code"], "unknown_node_type", "{}", v);
    // X-313: create refuses the same unknown type.
    let (s, v) = g.post("/flow/api/workflows", &bad).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(v["error"], "Invalid workflow structure");
    assert_eq!(v["errors"][0]["code"], "unknown_node_type", "{}", v);
}

/// IE-08: the import route takes a workflow at the node cap past the app's
/// default body size, and refuses one node more by the validator's rule.
#[tokio::test]
async fn a_workflow_at_the_node_cap_is_imported_past_the_default_body_size() {
    let g = G::new();
    let text = "x".repeat(4000);
    let mut nodes = vec![n("s", "start", json!({"scheduleType": "manual"}))];
    let mut edges = Vec::new();
    for i in 1..500 {
        nodes.push(n(&format!("l{}", i), "log", json!({"message": text})));
        edges.push(e("s", &format!("l{}", i)));
    }
    let body = json!({"name": "Wide", "nodes": nodes, "edges": edges});
    assert!(
        body.to_string().len() > 1024 * 1024,
        "past the 1 MB default"
    );
    let (s, v) = g.post("/flow/api/workflows/import", &body).await;
    assert_eq!(s, StatusCode::CREATED, "{}", v);
    let mut over = body;
    over["nodes"]
        .as_array_mut()
        .unwrap()
        .push(n("l500", "log", json!({"message": "one more"})));
    let (s, v) = g.post("/flow/api/workflows/import", &over).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(v["message"], "Workflow has more than 500 nodes");
}

/// IE-13, IE-14, V-139: replace refuses an empty body and an invalid graph,
/// migrates a legacy one, sets and clears the description, and keeps the
/// webhook settings and the run history.
#[tokio::test]
async fn replace_keeps_what_is_not_the_graph() {
    let g = G::new();
    let id = g.create(runnable("Keep")).await;
    let path = format!("/flow/api/workflows/{}/replace", id);
    let (s, _) = g
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/execute", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, hook) = g
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/webhook/enable", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", hook);
    let (s, _) = g
        .post(
            &format!("/flow/api/workflows/{}/webhook/auth-type", id),
            &json!({"auth_type": "url"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    for empty in ["{}", "null", ""] {
        let (s, v) = g.call(Method::POST, &path, Some(empty)).await;
        assert_eq!(
            (s, v),
            (
                StatusCode::BAD_REQUEST,
                json!({"error": "No data provided"})
            ),
            "{:?}",
            empty
        );
    }
    let mut bad = runnable("Keep");
    bad["nodes"][1]["type"] = json!("Nope");
    let (s, v) = g.post(&path, &bad).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(
        (
            v["status"].clone(),
            v["error"].clone(),
            v["errors"][0]["code"].clone()
        ),
        (
            json!("error"),
            json!("Invalid workflow format"),
            json!("unknown_node_type")
        )
    );
    let mut next = legacy("Kept name");
    next["description"] = json!("Exit at 15:10");
    let (s, v) = g.post(&path, &next).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["migrations"], json!([LEGACY_NOTE]));
    let (_, one) = g
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(one["name"], "Kept name");
    assert_eq!(one["description"], "Exit at 15:10");
    assert_eq!(one["nodes"][1]["data"]["condition"], "quantity_above");
    assert_eq!(one["webhook_enabled"], true);
    assert_eq!(one["webhook_auth_type"], "url");
    assert_eq!(one["webhook_token"], hook["webhook_token"]);
    assert_eq!(one["is_active"], false);
    let mut cleared = legacy("Kept name");
    cleared["description"] = json!("");
    let (s, _) = g.post(&path, &cleared).await;
    assert_eq!(s, StatusCode::OK);
    let (_, one) = g
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(one["description"], "");
    let (_, runs) = g
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/executions", id),
            None,
        )
        .await;
    assert_eq!(
        runs.as_array().map(Vec::len),
        Some(1),
        "the history is kept"
    );
    g.h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER frozen BEFORE UPDATE ON flow_workflows
             BEGIN SELECT RAISE(ABORT, 'held'); END;",
        )
        .unwrap();
    let (s, v) = g.post(&path, &runnable("Keep")).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to replace workflow"})
        )
    );
}

/// R-34, V-135: run now without an API key is the web's 400; X-175: the
/// history limit reads as the web reads it (missing, unreadable or 0 is 20).
#[tokio::test]
async fn run_now_needs_a_key_and_history_defaults_to_twenty() {
    let g = G::new();
    let id = g.create(runnable("Keyless")).await;
    {
        let ctx = g.h.ctx();
        let conn = ctx.sqlite.conn().unwrap();
        for _ in 0..25 {
            crate::flow::store::executions::create(&conn, id, ctx.now()).unwrap();
        }
    }
    for (query, want) in [
        ("", 20),
        ("?limit=0", 20),
        ("?limit=abc", 20),
        ("?limit=3", 3),
        ("?limit=1000", 25),
    ] {
        let (s, v) = g
            .call(
                Method::GET,
                &format!("/flow/api/workflows/{}/executions{}", id, query),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v.as_array().map(Vec::len), Some(want), "{}", query);
    }
    g.h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute_batch("DELETE FROM api_keys")
        .unwrap();
    let (s, v) = g
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/execute", id),
            None,
        )
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::BAD_REQUEST,
            json!({"error": "API key not configured"})
        )
    );
}

/// X-328: run now on a workflow that waits answers 202 at once, the run
/// going on as an owned task.
#[tokio::test]
async fn run_now_on_a_waiting_workflow_is_accepted() {
    let g = G::new();
    let id = g
        .create(
            json!({"name": "Waits", "nodes": [n("s", "start", json!({"scheduleType": "manual"})),
                        n("d", "delay", json!({"delayValue": 60}))], "edges": [e("s", "d")]}),
        )
        .await;
    let (s, v) = g
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/execute", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{}", v);
    assert_eq!(v["status"], "accepted");
    assert_eq!(
        v["message"],
        crate::flow::executor::waits::STARTED_IN_BACKGROUND
    );
    g.h.ctx().flow.stop_all(crate::flow::STOP_GRACE).await;
}

/// V-141: export, the webhook settings, deactivate and delete never ask
/// the validator, so they work on a workflow that could not run.
#[tokio::test]
async fn an_unrunnable_workflow_is_still_managed() {
    let g = G::new();
    let id = g.create(json!({"name": "Draft"})).await;
    let base = format!("/flow/api/workflows/{}", id);
    let (s, v) = g.call(Method::GET, &format!("{}/export", base), None).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["name"], "Draft");
    for (method, path, body) in [
        (Method::GET, format!("{}/webhook", base), None),
        (Method::POST, format!("{}/webhook/enable", base), None),
        (
            Method::POST,
            format!("{}/webhook/auth-type", base),
            Some(r#"{"auth_type": "url"}"#),
        ),
        (Method::POST, format!("{}/webhook/regenerate", base), None),
        (
            Method::POST,
            format!("{}/webhook/regenerate-secret", base),
            None,
        ),
        (Method::POST, format!("{}/webhook/disable", base), None),
        (Method::POST, format!("{}/deactivate", base), None),
        (Method::DELETE, base.clone(), None),
    ] {
        let (s, v) = g.call(method, &path, body).await;
        assert_eq!(s, StatusCode::OK, "{} {}", path, v);
    }
}

/// V-132: a PUT to a workflow that does not exist is checked first: an
/// invalid graph is the validator's 400, a valid edit the 404.
#[tokio::test]
async fn an_update_checks_the_graph_before_the_workflow_exists() {
    let g = G::new();
    let path = "/flow/api/workflows/9999";
    let (s, v) = g.call(Method::PUT, path, Some(r#"{"nodes": "x"}"#)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(v["error"], "Invalid workflow structure");
    let (s, v) = g.call(Method::PUT, path, Some(r#"{"name": "x"}"#)).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"error": "Workflow not found"})
        )
    );
}
