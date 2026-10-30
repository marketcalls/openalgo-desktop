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
}

/// WEB-DEFECT R-16: a null name is the default, never a failed insert (the
/// web answers 500).
#[tokio::test]
async fn a_null_name_is_the_default() {
    let f = F::new();
    let v = f.create(json!({"name": null})).await;
    assert_eq!(v["name"], "Untitled Workflow");
}

/// WEB-DEFECT V-131 (decision 23): create checks the graph it stores, as
/// sent: a falsy non-list is refused, never stored or swapped for `[]`;
/// absent or null is the empty list.
#[tokio::test]
async fn create_checks_the_graph_it_stores() {
    let f = F::new();
    for (graph, msg) in [
        (
            json!({"name": "x", "nodes": {}}),
            "Workflow needs a nodes array",
        ),
        (
            json!({"name": "x", "nodes": [], "edges": ""}),
            "Workflow needs an edges array",
        ),
    ] {
        let (s, v) = f
            .call(
                Method::POST,
                "/flow/api/workflows",
                Some(&graph.to_string()),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
        assert_eq!(v["error"], "Invalid workflow structure", "{}", v);
        assert_eq!(v["message"], msg, "{}", v);
    }
    let v = f.create(json!({"name": "x", "nodes": null})).await;
    assert_eq!(
        (v["nodes"].clone(), v["edges"].clone()),
        (json!([]), json!([]))
    );
}

/// WEB-DEFECT IE-06: an import body that is not an object is a 400 (the
/// web answers 500).
#[tokio::test]
async fn an_import_that_is_not_an_object_is_refused() {
    let f = F::new();
    let (s, v) = f
        .call(Method::POST, "/flow/api/workflows/import", Some("[1]"))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
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

/// R-14, R-18, IE-06, IE-13 (decision 24): a body that is not JSON is the
/// web's "No data provided" on every session JSON route; one with NaN or
/// Infinity keeps decision 15's own sentence.
#[tokio::test]
async fn a_body_that_is_not_json_has_no_data() {
    let f = F::new();
    let id = f.create(graph()).await["id"].as_i64().unwrap();
    let routes = [
        (
            Method::POST,
            "/flow/api/workflows".to_string(),
            "Invalid workflow structure",
        ),
        (
            Method::PUT,
            format!("/flow/api/workflows/{}", id),
            "Invalid workflow structure",
        ),
        (
            Method::POST,
            "/flow/api/workflows/import".to_string(),
            "Invalid workflow format",
        ),
        (
            Method::POST,
            format!("/flow/api/workflows/{}/replace", id),
            "Invalid workflow structure",
        ),
    ];
    for (method, path, title) in routes {
        for text in ["BUY NIFTY", "{\"name\": "] {
            let (s, v) = f.call(method.clone(), &path, Some(text)).await;
            assert_eq!(
                (s, v),
                (
                    StatusCode::BAD_REQUEST,
                    json!({"error": "No data provided"})
                ),
                "{} {}",
                path,
                text
            );
        }
        let (s, v) = f
            .call(
                method.clone(),
                &path,
                Some("{\"name\": \"x\", \"nodes\": [NaN]}"),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", path);
        assert_eq!(
            (v["error"].clone(), v["message"].clone()),
            (
                json!(title),
                json!("The workflow is not valid JSON (values such as NaN or Infinity are not allowed). Check it and try again.")
            ),
            "{}",
            path
        );
    }
}

/// R-05 (decision 23): a 404 under the webhook address never echoes the
/// token, as the web's `redact_url_credentials` masks it.
#[tokio::test]
async fn an_unknown_webhook_path_does_not_echo_its_token() {
    let f = F::new();
    for (method, path, shown) in [
        (
            Method::GET,
            "/flow/webhook/tok3n-abc/SBIN/x",
            "/flow/webhook/<redacted>/SBIN/x",
        ),
        (
            Method::POST,
            "/flow/webhook/tok3n-abc/SBIN/x",
            "/flow/webhook/<redacted>/SBIN/x",
        ),
        (
            Method::GET,
            "/flow/webhook/tok3n-abc/SBIN/x?secret=s",
            "/flow/webhook/<redacted>/SBIN/x",
        ),
        (
            Method::POST,
            "/FLOW/WEBHOOK/tok3n-abc/x/y",
            "/FLOW/WEBHOOK/<redacted>/x/y",
        ),
    ] {
        let (s, v) = f.call(method, path, None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{}", path);
        assert_eq!(
            v,
            json!({"status": "error", "message": "Not found", "path": shown}),
            "{}",
            path
        );
    }
}

/// F-101 / F-409 (decision 28): the editor's address takes a workflow
/// number only, as the web's `<int:workflow_id>`; anything else is the
/// app's 404 page.
#[tokio::test]
async fn an_editor_address_without_a_number_is_not_a_page() {
    let f = F::new();
    for p in ["/flow/editor/abc", "/flow/editor/-1", "/flow/editor/7x"] {
        let (s, _, _) = f.h.send(get(p)).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{}", p);
    }
    let (s, _, _) = f.h.send(get("/flow/editor/12")).await;
    assert_eq!(s, StatusCode::OK);
}
