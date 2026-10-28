//! The external data routes against the web's `get_mcp_servers` and
//! `test_mcp_servers`: the paths, the envelopes, the refusals, and a
//! connection test against stand-in servers behind the transport seam.

use super::*;
use crate::agent::mcp::testing::{
    demo_servers, install, reply_raw, respond, FakeClock, FakeServer, Seen,
};

fn call(method: Method, path: &str, body: Option<&str>, cookie: &str, csrf: &str) -> Request<Body> {
    let r = Request::builder()
        .method(method)
        .uri(path)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            body.map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    with_session(r, cookie, Some(csrf))
}

#[tokio::test]
async fn the_external_data_routes_answer_as_the_web() {
    let table = routes::table();
    for (method, path) in [
        (Method::GET, "/agent/api/mcp"),
        (Method::POST, "/agent/api/mcp/test"),
    ] {
        assert!(
            table.iter().any(|r| r.method == method && r.path == path),
            "{} {}",
            method,
            path
        );
    }
    let h = H::new();
    let (s, _) = h.json(get("/agent/api/mcp")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (c, t) = h.session(true);
    // Nothing registered: the switch, on by default, and no server.
    let (s, v) = h
        .json(call(Method::GET, "/agent/api/mcp", None, &c, &t))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "enabled": true, "servers": []})
        )
    );
    for (body, status, message) in [
        (
            Some("[1]"),
            StatusCode::BAD_REQUEST,
            Some("The request body must be a JSON object"),
        ),
        (
            Some(r#"{"server": "nope"}"#),
            StatusCode::BAD_REQUEST,
            Some("Unknown external data server"),
        ),
        (
            Some(r#"{"server": 5}"#),
            StatusCode::BAD_REQUEST,
            Some("Unknown external data server"),
        ),
        (Some("{}"), StatusCode::OK, None),
        (None, StatusCode::OK, None),
    ] {
        let (s, v) = h
            .json(call(Method::POST, "/agent/api/mcp/test", body, &c, &t))
            .await;
        assert_eq!(s, status, "{:?}", body);
        match message {
            Some(m) => assert_eq!(v, json!({"status": "error", "message": m})),
            None => assert_eq!(v, json!({"status": "success", "results": []})),
        }
    }
}

#[tokio::test]
async fn a_connection_test_reports_each_server_in_a_200() {
    let h = H::new();
    let (c, t) = h.session(true);
    let server = Arc::new(FakeServer::default());
    server.on(
        "initialize",
        respond(|server: &FakeServer, seen: &Seen| {
            if seen.url.contains("demo_live") {
                return Ok(reply_raw(503, &[], "down"));
            }
            server.default_reply(seen)
        }),
    );
    install(
        h.ctx().agent.mcp(),
        demo_servers(),
        &server,
        &Arc::new(FakeClock::default()),
    );
    let (_, v) = h
        .json(call(Method::GET, "/agent/api/mcp", None, &c, &t))
        .await;
    assert_eq!(
        v["servers"][0],
        json!({"key": "demo_eod", "title": "Demo end-of-day data", "description": "Stand-in server for tests.", "url": "https://example.invalid/demo_eod/mcp"})
    );
    let (s, v) = h
        .json(call(
            Method::POST,
            "/agent/api/mcp/test",
            Some(r#"{"server": "DEMO_LIVE"}"#),
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        (results[0]["key"].clone(), results[0]["ok"].clone()),
        (json!("demo_live"), json!(false))
    );
    let (_, v) = h
        .json(call(
            Method::POST,
            "/agent/api/mcp/test",
            Some("{}"),
            &c,
            &t,
        ))
        .await;
    let first = &v["results"][0];
    assert_eq!(first["ok"], true);
    assert_eq!(first["tool_count"], 2);
    assert_eq!(first["server_name"], "fake-server");
    assert_eq!(
        first["message"],
        "Demo end-of-day data answered and offers 2 tools."
    );
    let mut keys: Vec<&str> = first
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "key",
            "latency_ms",
            "message",
            "ok",
            "server_name",
            "title",
            "tool_count"
        ]
    );
}
