//! Flow's workflow routes, the rules the wave-6 sweep found unpinned: the
//! validator's level on each route (a graph only the stricter level
//! refuses tells them apart), the merge of a PUT with the stored half, the
//! database's refusal of a create, the export stamp, import names, the
//! CSRF token on PUT and DELETE, and the API key column after activation.

use super::*;

/// A signed-in browser session over the whole router.
struct W {
    h: H,
    cookie: String,
    csrf: String,
}

impl W {
    fn new() -> Self {
        let h = H::new();
        h.setup();
        let (cookie, csrf) = h.session(true);
        W { h, cookie, csrf }
    }

    fn request(method: Method, path: &str, body: Option<&str>) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ACCEPT, "application/json");
        if body.is_some() {
            b = b.header(header::CONTENT_TYPE, "application/json");
        }
        b.body(Body::from(body.unwrap_or("").to_string())).unwrap()
    }

    async fn call(&self, method: Method, path: &str, body: Option<&str>) -> (StatusCode, Value) {
        let req = Self::request(method, path, body);
        self.h
            .json(with_session(req, &self.cookie, Some(&self.csrf)))
            .await
    }

    async fn post(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        self.call(Method::POST, path, Some(&body.to_string())).await
    }

    async fn put(&self, id: i64, body: &Value) -> (StatusCode, Value) {
        let path = format!("/flow/api/workflows/{}", id);
        self.call(Method::PUT, &path, Some(&body.to_string())).await
    }

    async fn create(&self, body: Value) -> i64 {
        let (s, v) = self.post("/flow/api/workflows", &body).await;
        assert_eq!(s, StatusCode::CREATED, "{}", v);
        v["id"].as_i64().unwrap()
    }

    async fn get(&self, id: i64) -> Value {
        let path = format!("/flow/api/workflows/{}", id);
        let (s, v) = self.call(Method::GET, &path, None).await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        v
    }

    async fn list(&self) -> Value {
        self.call(Method::GET, "/flow/api/workflows", None).await.1
    }

    fn sql(&self, batch: &str) {
        self.h
            .ctx()
            .sqlite
            .conn()
            .unwrap()
            .execute_batch(batch)
            .unwrap();
    }
}

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

fn runnable(name: &str) -> Value {
    json!({"name": name, "nodes": [start(), log("hello")], "edges": [e("s", "l")]})
}

const NO_TRIGGER: &str = "Workflow has no trigger node, so it can never run. Add one of: orderUpdateTrigger, priceAlert, start, webhookTrigger";
const UNREACHABLE: &str = "These nodes cannot be reached from the trigger and will never run: l";

/// Graphs the editor saves while wiring them and only the strict level
/// refuses, with the strict level's first error (code, message).
fn unfinished() -> Vec<(Value, &'static str, &'static str)> {
    vec![
        (
            json!({"nodes": [log("hello")], "edges": []}),
            "no_trigger",
            NO_TRIGGER,
        ),
        (
            json!({"nodes": [start(), log("hello")], "edges": []}),
            "unreachable",
            UNREACHABLE,
        ),
    ]
}

/// The web's refusal of a workflow without a usable name.
fn no_name(received: Option<Value>) -> Value {
    let mut err = json!({"path": "/name", "code": "required",
                         "message": "Workflow needs a non-empty name", "expected": "string"});
    if let Some(r) = received {
        err["received"] = r;
    }
    err
}

/// V-130 (require_name=False): create checks a finished graph without
/// asking for a name; an absent name is the web's default, an empty one is
/// stored as sent.
#[tokio::test]
async fn create_takes_a_finished_graph_without_a_name() {
    let w = W::new();
    let mut body = runnable("");
    for (name, stored) in [(None, "Untitled Workflow"), (Some(""), "")] {
        match name {
            None => body.as_object_mut().unwrap().remove("name"),
            Some(n) => body
                .as_object_mut()
                .unwrap()
                .insert("name".into(), json!(n)),
        };
        let (s, v) = w.post("/flow/api/workflows", &body).await;
        assert_eq!(s, StatusCode::CREATED, "{:?} {}", name, v);
        assert_eq!(v["name"], stored);
        assert_eq!(v["nodes"], body["nodes"]);
    }
}

/// V-130 (strict=False): create keeps a half-built graph (no trigger, a
/// node the trigger cannot reach) that import, held to the strict level,
/// refuses.
#[tokio::test]
async fn create_takes_a_graph_that_is_not_finished() {
    let w = W::new();
    for (mut graph, code, message) in unfinished() {
        graph["name"] = json!("Draft");
        let (s, v) = w.post("/flow/api/workflows", &graph).await;
        assert_eq!(s, StatusCode::CREATED, "{}", v);
        assert_eq!(v["nodes"], graph["nodes"]);
        let (s, v) = w.post("/flow/api/workflows/import", &graph).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
        assert_eq!(
            (v["errors"][0]["code"].clone(), v["message"].clone()),
            (json!(code), json!(message))
        );
    }
}

/// V-132 (require_name=False): a PUT's graph is checked without asking for
/// a name, so a workflow saved with an empty name keeps saving.
#[tokio::test]
async fn an_update_needs_no_name() {
    let w = W::new();
    let id = w.create(json!({"name": ""})).await;
    let g = runnable("");
    for body in [
        json!({"nodes": g["nodes"], "edges": g["edges"]}),
        json!({"name": "", "nodes": g["nodes"]}),
    ] {
        let (s, v) = w.put(id, &body).await;
        assert_eq!(s, StatusCode::OK, "{} {}", body, v);
        assert_eq!(
            (v["name"].clone(), v["nodes"].clone()),
            (json!(""), g["nodes"].clone())
        );
    }
}

/// V-132 (strict=False): a PUT keeps a half-built graph (no trigger, a
/// node the trigger cannot reach), as the editor saves while wiring.
#[tokio::test]
async fn an_update_keeps_a_graph_that_is_not_finished() {
    let w = W::new();
    let id = w.create(runnable("Draft")).await;
    for (graph, _, _) in unfinished() {
        let (s, v) = w.put(id, &graph).await;
        assert_eq!(s, StatusCode::OK, "{} {}", graph, v);
        assert_eq!(v["nodes"], graph["nodes"]);
        assert_eq!(v["edges"], json!([]));
    }
}

/// An unrunnable workflow (`path` is `execute` or `activate`) is refused
/// for its graph with the key, and for the key without it.
async fn key_comes_before_the_graph(path: &str, refused: &str) {
    let w = W::new();
    let id = w.create(json!({"name": "Draft"})).await;
    let route = format!("/flow/api/workflows/{}/{}", id, path);
    let (s, v) = w.call(Method::POST, &route, None).await;
    assert_eq!(
        (s, v["error"].clone(), v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!(refused),
            json!("Workflow has no nodes, so it can never run.")
        )
    );
    w.sql("DELETE FROM api_keys");
    let (s, v) = w.call(Method::POST, &route, None).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::BAD_REQUEST,
            json!({"error": "API key not configured"})
        )
    );
}

/// V-135: Run Now asks for the API key before it checks the graph.
#[tokio::test]
async fn run_now_asks_for_the_key_before_the_graph() {
    key_comes_before_the_graph("execute", "Workflow cannot be executed").await;
}

/// V-135: activation asks for the API key before it checks the graph, in
/// the web's same order.
#[tokio::test]
async fn activation_asks_for_the_key_before_the_graph() {
    key_comes_before_the_graph("activate", "Workflow cannot be activated").await;
}

/// V-137, IE-06 (require_name=True): import refuses a finished graph whose
/// name is absent, null, empty, blank or not text, with the validator's
/// error, and stores nothing.
#[tokio::test]
async fn an_import_needs_a_name() {
    let w = W::new();
    for (name, received) in [
        (None, None),
        (Some(Value::Null), None),
        (Some(json!("")), Some(json!(""))),
        (Some(json!("   ")), Some(json!("   "))),
        (Some(json!(5)), Some(json!(5))),
    ] {
        let mut body = runnable("");
        match name {
            None => body.as_object_mut().unwrap().remove("name"),
            Some(v) => body.as_object_mut().unwrap().insert("name".into(), v),
        };
        let (s, v) = w.post("/flow/api/workflows/import", &body).await;
        assert_eq!(
            (s, v),
            (
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "error": "Invalid workflow format",
                       "message": "Workflow needs a non-empty name",
                       "errors": [no_name(received)]})
            ),
            "{}",
            body
        );
    }
    assert_eq!(w.list().await, json!([]));
}

/// V-138, IE-06 (WEB-DEFECT: the web fails these with a 500): an import
/// body that is JSON but not an object is the validator's refusal, in
/// full, named with Python's type.
#[tokio::test]
async fn an_import_that_is_not_an_object_is_the_validators_refusal() {
    let w = W::new();
    for (body, received) in [
        ("[1]", "list"),
        ("\"text\"", "str"),
        ("5", "int"),
        ("1.5", "float"),
        ("true", "bool"),
    ] {
        let (s, v) = w
            .call(Method::POST, "/flow/api/workflows/import", Some(body))
            .await;
        assert_eq!(
            (s, v),
            (
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "error": "Invalid workflow format",
                       "message": "Workflow must be a JSON object",
                       "errors": [{"path": "/", "code": "invalid_type",
                                   "message": "Workflow must be a JSON object",
                                   "expected": "object", "received": received}]})
            ),
            "{}",
            body
        );
    }
}

/// V-139, IE-13 (strict=True): replace refuses a graph only the strict
/// level refuses, and checks the stored name when none is sent.
#[tokio::test]
async fn replace_holds_the_graph_to_the_strict_level() {
    let w = W::new();
    let id = w.create(runnable("Keep")).await;
    let path = format!("/flow/api/workflows/{}/replace", id);
    for (graph, code, message) in unfinished() {
        let (s, v) = w.post(&path, &graph).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
        assert_eq!(
            (
                v["error"].clone(),
                v["message"].clone(),
                v["errors"][0]["code"].clone()
            ),
            (
                json!("Invalid workflow format"),
                json!(message),
                json!(code)
            )
        );
    }
    assert_eq!(w.get(id).await["nodes"], runnable("Keep")["nodes"]);
    let unnamed = w.create(json!({"name": ""})).await;
    let (s, v) = w
        .post(
            &format!("/flow/api/workflows/{}/replace", unnamed),
            &runnable(""),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(v["errors"], json!([no_name(Some(json!("")))]));
}

/// V-139 (`nodes or []`): replace reads falsy nodes as none, so they are
/// refused as an empty graph, never as a malformed one.
#[tokio::test]
async fn replace_reads_falsy_nodes_as_none() {
    let w = W::new();
    let id = w.create(runnable("Keep")).await;
    let path = format!("/flow/api/workflows/{}/replace", id);
    for nodes in [json!({}), json!(""), json!(0), json!(false), Value::Null] {
        let (s, v) = w.post(&path, &json!({"nodes": nodes, "edges": []})).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{} {}", nodes, v);
        assert_eq!(
            v["errors"],
            json!([{"path": "/nodes", "code": "no_trigger",
                    "message": "Workflow has no nodes, so it can never run.",
                    "expected": "at least a trigger node", "received": 0}]),
            "{}",
            nodes
        );
    }
}

/// R-20, V-132: a PUT of nodes alone is checked with the stored edges:
/// dropping a node a stored edge still points to is refused (an empty
/// edge list would pass it), and nothing is written.
#[tokio::test]
async fn a_put_of_nodes_is_checked_with_the_stored_edges() {
    let w = W::new();
    let id = w.create(runnable("Wired")).await;
    let (s, v) = w.put(id, &json!({"nodes": [start()]})).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::BAD_REQUEST,
            json!({"status": "error", "error": "Invalid workflow structure",
                   "message": "Edge target 'l' is not a node in this workflow",
                   "errors": [{"path": "/edges/0/target", "code": "dangling_edge",
                               "message": "Edge target 'l' is not a node in this workflow",
                               "expected": "an existing node id", "received": "l"}]})
        )
    );
    assert_eq!(w.get(id).await["nodes"], runnable("Wired")["nodes"]);
}

/// R-20, V-132: a PUT of edges alone is checked with the stored nodes: an
/// edge to a node the workflow does not have is refused (an empty node
/// list would pass it), and nothing is written.
#[tokio::test]
async fn a_put_of_edges_is_checked_with_the_stored_nodes() {
    let w = W::new();
    let id = w.create(runnable("Wired")).await;
    let (s, v) = w.put(id, &json!({"edges": [e("s", "x")]})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(
        v["errors"],
        json!([{"path": "/edges/0/target", "code": "dangling_edge",
                "message": "Edge target 'x' is not a node in this workflow",
                "expected": "an existing node id", "received": "x"}])
    );
    assert_eq!(w.get(id).await["edges"], json!([e("s", "l")]));
}

/// R-16: a create the database refuses is the web's 500, in its words,
/// whether or not a graph was sent; nothing is stored.
#[tokio::test]
async fn a_create_the_database_refuses_is_a_500() {
    let w = W::new();
    w.sql(
        "CREATE TRIGGER no_new BEFORE INSERT ON flow_workflows
         BEGIN SELECT RAISE(ABORT, 'held'); END;",
    );
    for body in [json!({"name": "Held"}), runnable("Held")] {
        let (s, v) = w.post("/flow/api/workflows", &body).await;
        assert_eq!(
            (s, v),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "Failed to create workflow"})
            ),
            "{}",
            body
        );
    }
    assert_eq!(w.list().await, json!([]));
}

/// V-139 (`edges or []`): replace reads absent or falsy edges as none and
/// stores that, so the stored edges never outlive the graph they wired.
#[tokio::test]
async fn replace_reads_absent_or_falsy_edges_as_none() {
    let w = W::new();
    let path_of = |id: i64| format!("/flow/api/workflows/{}/replace", id);
    let mut cases = vec![json!({"nodes": [start()]})];
    for edges in [json!({}), json!(""), json!(0), Value::Null] {
        cases.push(json!({"nodes": [start()], "edges": edges}));
    }
    for body in cases {
        let id = w.create(runnable("Wired")).await;
        let (s, v) = w.post(&path_of(id), &body).await;
        assert_eq!(s, StatusCode::OK, "{} {}", body, v);
        let stored = w.get(id).await;
        assert_eq!(
            (stored["nodes"].clone(), stored["edges"].clone()),
            (json!([start()]), json!([])),
            "{}",
            body
        );
    }
}

/// IE-14: replace keeps the stored description when the file has none or
/// a null one (the web's `is not None`).
#[tokio::test]
async fn replace_keeps_the_description_when_none_is_sent() {
    let w = W::new();
    let mut described = runnable("Keep");
    described["description"] = json!("Exit at 15:10");
    let id = w.create(described).await;
    let path = format!("/flow/api/workflows/{}/replace", id);
    let mut next = runnable("Keep");
    for description in [None, Some(Value::Null)] {
        if let Some(d) = description {
            next["description"] = d;
        }
        let (s, v) = w.post(&path, &next).await;
        assert_eq!(s, StatusCode::OK, "{} {}", next, v);
        assert_eq!(w.get(id).await["description"], "Exit at 15:10", "{}", next);
    }
}

/// IE-01: `exported_at` is `datetime.utcnow().isoformat()`: the UTC wall
/// time (the clock reads 10:00 IST), no zone, microseconds as six digits
/// when there are any and left out when there are none; below a
/// microsecond is dropped, as Python's clock reading floors.
#[tokio::test]
async fn the_export_is_stamped_as_python_writes_utc_now() {
    let w = W::new();
    let id = w.create(runnable("Stamped")).await;
    let path = format!("/flow/api/workflows/{}/export", id);
    for (now, want) in [
        ("2026-10-05T04:30:00Z", "2026-10-05T04:30:00"),
        ("2026-10-05T04:45:00.1Z", "2026-10-05T04:45:00.100000"),
        ("2026-10-05T04:45:00.000042Z", "2026-10-05T04:45:00.000042"),
        (
            "2026-10-05T04:45:00.123456789Z",
            "2026-10-05T04:45:00.123456",
        ),
        ("2026-10-05T04:45:00.000000999Z", "2026-10-05T04:45:00"),
    ] {
        w.h.t.clock.set(now.parse().unwrap());
        let (s, v) = w.call(Method::GET, &path, None).await;
        assert_eq!(
            (s, v["exported_at"].clone()),
            (StatusCode::OK, json!(want)),
            "{}",
            now
        );
    }
}

/// IE-07: an import whose name a workflow already has is another workflow
/// with the same name (the web checks no collision).
#[tokio::test]
async fn an_import_with_a_taken_name_is_another_workflow() {
    let w = W::new();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let (s, v) = w
            .post("/flow/api/workflows/import", &runnable("Same"))
            .await;
        assert_eq!(s, StatusCode::CREATED, "{}", v);
        ids.push(v["workflow_id"].as_i64().unwrap());
    }
    assert_ne!(ids[0], ids[1]);
    for id in ids {
        assert_eq!(w.get(id).await["name"], "Same (imported)");
    }
}

/// IE-07: a long name is imported whole (the web caps none).
#[tokio::test]
async fn an_import_with_a_long_name_keeps_it_whole() {
    let w = W::new();
    let name = "Nifty opening range breakout ".repeat(200);
    let (s, v) = w.post("/flow/api/workflows/import", &runnable(&name)).await;
    assert_eq!(s, StatusCode::CREATED, "{}", v);
    let id = v["workflow_id"].as_i64().unwrap();
    assert_eq!(w.get(id).await["name"], format!("{} (imported)", name));
}
