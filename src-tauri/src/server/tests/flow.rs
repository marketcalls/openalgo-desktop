//! Flow's workflow routes over HTTP, against the web's `blueprints/flow.py`
//! (parity R-13..R-38, IE-01..IE-15): status codes, JSON shapes, messages,
//! the session rule, and a web export round-tripping unchanged.

use super::*;

const EXPORT_FROM_WEB: &str = r##"{
  "name": "EMA crossover",
  "description": "From the web",
  "version": "1.0",
  "exported_at": "2026-10-29T10:00:00.123456",
  "nodes": [
    {"id": "start-1", "type": "start", "position": {"x": 100, "y": 50},
     "data": {"label": "Start", "scheduleType": "manual"},
     "measured": {"width": 180, "height": 60}, "selected": false, "dragging": false},
    {"id": "log-1", "type": "log", "position": {"x": 100, "y": 200},
     "data": {"message": "Ran on {{date}}", "level": "info", "futureField": [1, 2]}}
  ],
  "edges": [
    {"id": "e1", "source": "start-1", "target": "log-1", "type": "insertable",
     "animated": true, "style": {"stroke": "#3b82f6"}, "sourceHandle": null}
  ]
}"##;

struct F {
    h: H,
    cookie: String,
    csrf: String,
}

impl F {
    fn new() -> Self {
        let h = H::new();
        h.setup();
        let (cookie, csrf) = h.session(true);
        F { h, cookie, csrf }
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

    async fn create(&self, body: Value) -> Value {
        let (s, v) = self
            .call(Method::POST, "/flow/api/workflows", Some(&body.to_string()))
            .await;
        assert_eq!(s, StatusCode::CREATED, "{}", v);
        v
    }
}

fn graph() -> Value {
    json!({
        "name": "Opening range",
        "nodes": [
            {"id": "s", "type": "start", "position": {"x": 0, "y": 0}, "data": {"scheduleType": "manual"}},
            {"id": "l", "type": "log", "position": {"x": 0, "y": 100}, "data": {"message": "hello"}}
        ],
        "edges": [{"id": "e", "source": "s", "target": "l"}]
    })
}

/// R-01 (the core part): the workflow routes, each for the signed-in user
/// only.
#[tokio::test]
async fn the_workflow_routes_are_the_webs_and_need_the_user() {
    let want = [
        ("GET", "/flow/api/workflows"),
        ("POST", "/flow/api/workflows"),
        ("POST", "/flow/api/workflows/import"),
        ("GET", "/flow/api/workflows/{id}"),
        ("PUT", "/flow/api/workflows/{id}"),
        ("DELETE", "/flow/api/workflows/{id}"),
        ("POST", "/flow/api/workflows/{id}/execute"),
        ("GET", "/flow/api/workflows/{id}/executions"),
        ("GET", "/flow/api/workflows/{id}/export"),
        ("POST", "/flow/api/workflows/{id}/replace"),
    ];
    let table = crate::server::routes::flow::table();
    let got: Vec<(String, &str)> = table
        .iter()
        .map(|r| (r.method.to_string(), r.path))
        .collect();
    for (m, p) in want {
        assert!(got.contains(&(m.to_string(), p)), "{} {} missing", m, p);
    }
    // Every route but the two public webhooks (the triggers lane's, each
    // with its own credential) is the signed-in user's.
    assert!(table
        .iter()
        .filter(|r| !r.path.starts_with("/flow/webhook/"))
        .all(|r| r.access == Access::User));
    let h = H::new();
    h.setup();
    let (s, _) = h.json(get("/flow/api/workflows")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

/// R-14..R-17: create answers 201 with the web's keys; a name-only body is
/// the editor's "New Strategy"; get adds `schedule_job_id`.
#[tokio::test]
async fn a_workflow_is_created_and_read_as_the_web_answers() {
    let f = F::new();
    let v = f.create(json!({"name": "New Strategy"})).await;
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "created_at",
            "description",
            "edges",
            "id",
            "is_active",
            "name",
            "nodes",
            "updated_at",
            "webhook_auth_type",
            "webhook_enabled",
            "webhook_secret",
            "webhook_token"
        ]
    );
    assert_eq!(
        (v["nodes"].clone(), v["edges"].clone()),
        (json!([]), json!([]))
    );
    assert_eq!(
        (v["is_active"].clone(), v["webhook_auth_type"].clone()),
        (json!(false), json!("payload"))
    );
    assert_eq!(v["webhook_secret"].as_str().map(str::len), Some(64));
    let id = v["id"].as_i64().unwrap();
    let (s, one) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(one["schedule_job_id"], Value::Null);
    assert_eq!(one["webhook_secret"], v["webhook_secret"]);
    let null_name = f.create(json!({"name": null})).await;
    assert_eq!(null_name["name"], "Untitled Workflow", "never a 500 (R-16)");
    let falsy = f
        .create(json!({"name": "x", "nodes": {}, "edges": ""}))
        .await;
    assert_eq!(
        (falsy["nodes"].clone(), falsy["edges"].clone()),
        (json!([]), json!([])),
        "what was checked is what is stored (V-131)"
    );
    let (s, v) = f.call(Method::GET, "/flow/api/workflows/9999", None).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"error": "Workflow not found"})
        )
    );
    let (s, v) = f.call(Method::GET, "/flow/api/workflows/abc", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["message"], "Not found");
}

#[tokio::test]
async fn bodies_are_refused_as_the_web_refuses_them() {
    let f = F::new();
    for empty in ["", "null", "{}", "[]", "0"] {
        let (s, v) = f
            .call(Method::POST, "/flow/api/workflows", Some(empty))
            .await;
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
    let (s, v) = f
        .call(Method::POST, "/flow/api/workflows", Some("[1]"))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"], "Invalid workflow structure");
    assert_eq!(v["message"], "Workflow must be a JSON object");
    assert_eq!(v["errors"][0]["received"], "list");
    let (s, v) = f
        .call(
            Method::POST,
            "/flow/api/workflows",
            Some(r#"{"name": "x", "nodes": "x"}"#),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Workflow needs a nodes array");
    let (s, v) = f
        .call(
            Method::POST,
            "/flow/api/workflows/import",
            Some("{\"nodes\": NaN}"),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "NaN is a 400, never a 500 (decision 15)"
    );
    assert_eq!(v["error"], "Invalid workflow format");
}

/// R-13: the list, newest change first, without secrets or graphs.
#[tokio::test]
async fn the_list_shows_the_last_run() {
    let f = F::new();
    let a = f.create(graph()).await["id"].as_i64().unwrap();
    f.create(json!({"name": "Second"})).await;
    let (s, _) = f
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/execute", a),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = f.call(Method::GET, "/flow/api/workflows", None).await;
    assert_eq!(s, StatusCode::OK);
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let ran = rows.iter().find(|r| r["id"] == a).unwrap();
    assert_eq!(ran["last_execution_status"], "completed");
    let mut keys: Vec<&str> = ran
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "created_at",
            "description",
            "id",
            "is_active",
            "last_execution_status",
            "name",
            "updated_at",
            "webhook_enabled"
        ]
    );
}

/// R-18..R-22: only editable fields change; the merged graph is checked.
#[tokio::test]
async fn an_update_changes_only_what_the_editor_may() {
    let f = F::new();
    let id = f.create(graph()).await["id"].as_i64().unwrap();
    let path = format!("/flow/api/workflows/{}", id);
    let (s, v) = f
        .call(
            Method::PUT,
            &path,
            Some(
                &json!({"name": "Renamed", "is_active": true, "webhook_enabled": true}).to_string(),
            ),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        (
            v["name"].clone(),
            v["is_active"].clone(),
            v["webhook_enabled"].clone()
        ),
        (json!("Renamed"), json!(false), json!(false))
    );
    assert_eq!(v["needs_reactivate"], false);
    assert_eq!(v["nodes"], graph()["nodes"], "a rename keeps the graph");
    let (s, v) = f.call(Method::PUT, &path, Some(r#"{"edges": null}"#)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Workflow needs an edges array");
    let (s, _) = f
        .call(
            Method::PUT,
            "/flow/api/workflows/9999",
            Some(r#"{"name": "x"}"#),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// R-34..R-38: Run Now answers the run; the history is newest first.
#[tokio::test]
async fn run_now_answers_the_run_and_records_it() {
    let f = F::new();
    let id = f.create(graph()).await["id"].as_i64().unwrap();
    let (s, run) = f
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/execute", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", run);
    assert_eq!(run["message"], "Workflow executed successfully");
    let (s, v) = f
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/executions?limit=abc", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v[0]["id"], run["execution_id"]);
    assert_eq!(v[0]["logs"], run["logs"]);
    let mut keys: Vec<&str> = v[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "completed_at",
            "error",
            "id",
            "logs",
            "started_at",
            "status",
            "workflow_id"
        ]
    );
    let (s, v) = f
        .call(Method::GET, "/flow/api/workflows/9999/executions", None)
        .await;
    assert_eq!((s, v), (StatusCode::OK, json!([])));
    let (s, v) = f
        .call(Method::POST, "/flow/api/workflows/9999/execute", None)
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"error": "Workflow not found"})
        )
    );
}

/// R-33/R-34: a workflow that is not runnable is a 400 before any run.
#[tokio::test]
async fn run_now_refuses_an_unrunnable_workflow() {
    let f = F::new();
    let id = f.create(json!({"name": "x"})).await["id"].as_i64().unwrap();
    let conn = f.h.ctx().sqlite.conn().unwrap();
    conn.execute(
        "UPDATE flow_workflows SET nodes = '\"x\"' WHERE id = ?1",
        [id],
    )
    .unwrap();
    drop(conn);
    let (s, v) = f
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/execute", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"], "Workflow cannot be executed");
    assert_eq!(v["message"], "Workflow needs a nodes array");
}

/// R-23: delete is refused while a run holds the workflow, then deletes it
/// with its history.
#[tokio::test]
async fn delete_waits_for_no_run() {
    let f = F::new();
    let id = f.create(graph()).await["id"].as_i64().unwrap();
    let path = format!("/flow/api/workflows/{}", id);
    let held = f.h.ctx().flow.try_claim(id).unwrap();
    let (s, v) = f.call(Method::DELETE, &path, None).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::CONFLICT,
            json!({"error": "This workflow is still running. Try again when it finishes."})
        )
    );
    drop(held);
    let (s, v) = f.call(Method::DELETE, &path, None).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Workflow deleted"})
        )
    );
    let (s, _) = f.call(Method::DELETE, &path, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// IE-01..IE-07: a web export imports unchanged, every unknown key kept,
/// and exports back with the web's keys.
#[tokio::test]
async fn a_web_export_round_trips_unchanged() {
    let f = F::new();
    let web: Value = serde_json::from_str(EXPORT_FROM_WEB).unwrap();
    let (s, v) = f
        .call(
            Method::POST,
            "/flow/api/workflows/import",
            Some(EXPORT_FROM_WEB),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", v);
    assert_eq!(v["status"], "success");
    assert!(v.get("migrations").is_none());
    let id = v["workflow_id"].as_i64().unwrap();
    let (s, out) = f
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/export", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let mut keys: Vec<&str> = out
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "description",
            "edges",
            "exported_at",
            "name",
            "nodes",
            "version"
        ]
    );
    assert_eq!(out["name"], "EMA crossover (imported)");
    assert_eq!(out["version"], "1.0");
    assert_eq!(out["nodes"], web["nodes"]);
    assert_eq!(out["edges"], web["edges"]);
    let (s, v) = f
        .call(Method::POST, "/flow/api/workflows/import", Some("[1]"))
        .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "an array is a 400, never the web's 500 (IE-06)"
    );
    assert_eq!(v["error"], "Invalid workflow format");
}

/// IE-13..IE-15: replace keeps the id and webhook credentials and swaps the
/// graph.
#[tokio::test]
async fn replace_swaps_the_graph_in_place() {
    let f = F::new();
    let created = f.create(graph()).await;
    let id = created["id"].as_i64().unwrap();
    let path = format!("/flow/api/workflows/{}/replace", id);
    let mut next = graph();
    next["nodes"][1]["data"]["message"] = json!("changed");
    next.as_object_mut().unwrap().remove("name");
    let (s, v) = f.call(Method::POST, &path, Some(&next.to_string())).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v,
        json!({"status": "success", "workflow_id": id, "migrations": [], "needs_reactivate": false,
               "message": "Workflow replaced. Changes apply from the next run."})
    );
    let (_, one) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(one["name"], "Opening range");
    assert_eq!(one["nodes"][1]["data"]["message"], "changed");
    assert_eq!(one["webhook_token"], created["webhook_token"]);
    assert_eq!(one["webhook_secret"], created["webhook_secret"]);
    let (s, v) = f.call(Method::POST, &path, Some("[1]")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        v,
        json!({"status": "error", "error": "Invalid workflow structure", "message": "Workflow must be a JSON object"})
    );
    let (s, _) = f
        .call(Method::POST, "/flow/api/workflows/9999/replace", Some("{}"))
        .await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "a missing workflow is checked before the body"
    );
}

/// R-06: the editor's pages are served by the app shell.
#[tokio::test]
async fn the_flow_pages_are_served() {
    let f = F::new();
    for p in ["/flow", "/flow/shortcuts", "/flow/editor/3"] {
        let (s, _, _) = f.h.send(get(p)).await;
        assert_eq!(s, StatusCode::OK, "{}", p);
    }
}

/// D-69: a workflow never holds a copy of the API key: create, import (even
/// one carrying an `api_key`) and a run leave the web's column empty, and
/// export never writes one. Runs read the trader's current key.
#[tokio::test]
async fn no_workflow_holds_an_api_key() {
    let f = F::new();
    let id = f.create(graph()).await["id"].as_i64().unwrap();
    let mut with_key = graph();
    with_key["api_key"] = json!("<APIKEY>");
    let (s, v) = f
        .call(
            Method::POST,
            "/flow/api/workflows/import",
            Some(&with_key.to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", v);
    let imported = v["workflow_id"].as_i64().unwrap();
    let (s, _) = f
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/execute", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, out) = f
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/export", imported),
            None,
        )
        .await;
    assert!(out.get("api_key").is_none());
    let c = f.h.ctx().sqlite.conn().unwrap();
    let held: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM flow_workflows WHERE api_key IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(held, 0);
}
