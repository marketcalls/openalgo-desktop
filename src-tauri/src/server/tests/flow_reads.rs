//! Flow's read routes when the database fails or there is nothing to
//! write (wave 6: R-13, R-19, R-38, S-11, S-12, S-28): the decided 500
//! (decision 34), a PUT of non-editable keys only, and the history cap.

use super::*;

struct R {
    h: H,
    cookie: String,
    csrf: String,
}

impl R {
    fn new() -> Self {
        let h = H::new();
        h.setup();
        let (cookie, csrf) = h.session(true);
        R { h, cookie, csrf }
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

    async fn create(&self, name: &str) -> i64 {
        let (s, v) = self
            .call(
                Method::POST,
                "/flow/api/workflows",
                Some(&json!({"name": name}).to_string()),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{}", v);
        v["id"].as_i64().unwrap()
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

/// R-13, S-11, S-12, S-28: a read the database cannot make is a 500 in
/// the route's words, never an empty list or a 404.
#[tokio::test]
async fn a_failed_read_is_a_500() {
    let r = R::new();
    let id = r.create("A").await;
    r.sql("ALTER TABLE flow_workflow_executions RENAME TO runs_gone;");
    let (s, v) = r
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/executions", id),
            None,
        )
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to read the run history"})
        )
    );
    r.sql(
        "ALTER TABLE runs_gone RENAME TO flow_workflow_executions;
           ALTER TABLE flow_workflows RENAME TO workflows_gone;",
    );
    let (s, v) = r.call(Method::GET, "/flow/api/workflows", None).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to list workflows"})
        )
    );
    let (s, v) = r
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to read the workflow"})
        )
    );
}

/// R-19: a PUT carrying only keys the editor may not change answers 200
/// and changes nothing: updated_at and the list order stay.
#[tokio::test]
async fn a_put_of_non_editable_keys_changes_nothing() {
    let r = R::new();
    let a = r.create("A").await;
    let b = r.create("B").await;
    r.sql(&format!(
        "UPDATE flow_workflows SET updated_at = '2026-10-29 03:45:00' WHERE id = {};
         UPDATE flow_workflows SET updated_at = '2026-10-29 04:45:00' WHERE id = {};",
        a, b
    ));
    let (s, v) = r
        .call(
            Method::PUT,
            &format!("/flow/api/workflows/{}", a),
            Some(r#"{"is_active": true, "webhook_enabled": true}"#),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["updated_at"], "2026-10-29T03:45:00");
    assert_eq!(v["is_active"], false);
    let (_, list) = r.call(Method::GET, "/flow/api/workflows", None).await;
    let order: Vec<i64> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![b, a]);
}

/// R-38: the history route answers at most 200 runs, however many are
/// asked for.
#[tokio::test]
async fn the_history_route_answers_at_most_200_runs() {
    let r = R::new();
    let id = r.create("A").await;
    r.sql(&format!(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 205)
         INSERT INTO flow_workflow_executions (workflow_id, status, started_at, logs)
         SELECT {}, 'completed', '2026-10-30 03:45:00', '[]' FROM n;",
        id
    ));
    for (query, want) in [
        ("?limit=1000", 200),
        ("?limit=200", 200),
        ("?limit=199", 199),
    ] {
        let (s, v) = r
            .call(
                Method::GET,
                &format!("/flow/api/workflows/{}/executions{}", id, query),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v.as_array().map(Vec::len), Some(want), "{}", query);
    }
}
