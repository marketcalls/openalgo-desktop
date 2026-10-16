//! DOC-06: every `/api/v1` method and path OpenAlgo web registers is served
//! here, apart from the exceptions named below. The list is
//! `tests/fixtures/web/route_inventory.json`, read from the web's source by
//! `tests/fixtures/web/capture/route_inventory.py`. API-01 (chart and the
//! Telegram bot management API) went unnoticed for want of this check.

use crate::api_v1_support::H;
use axum::body::Body;
use axum::http::{header, Method, Request};
use serde_json::{json, Value};

/// Web endpoints not served here, by path prefix, each with its reason.
/// An entry that starts being served fails the test until it is removed.
const NOT_SERVED: &[(&str, &str)] = &[
    (
        "/api/v1/portfolio/",
        "the Portfolio Backtester needs the web's pandas backtester: out of scope \
         (CLAUDE.md, scope decisions, 2026-10-03)",
    ),
    (
        "/api/v1/sip/",
        "the SIP Backtester needs pandas: out of scope (same decision)",
    ),
    (
        "/api/v1/telegram/broadcast",
        "API-01: the Telegram bot management API is not served yet",
    ),
    ("/api/v1/telegram/config", "API-01, as above"),
    ("/api/v1/telegram/preferences", "API-01, as above"),
    ("/api/v1/telegram/start", "API-01, as above"),
    ("/api/v1/telegram/stats", "API-01, as above"),
    ("/api/v1/telegram/stop", "API-01, as above"),
    ("/api/v1/telegram/users", "API-01, as above"),
    ("/api/v1/telegram/webhook", "API-01, as above"),
];

fn web_routes() -> Vec<(Method, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/web/route_inventory.json");
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let (method, path) = r.as_str().unwrap().split_once(' ').unwrap();
            // Flask's converter syntax; any symbol shows whether it routes.
            let path = path.replace("<string:symbol>", "NSE:SBIN");
            (method.parse().unwrap(), path)
        })
        .collect()
}

/// Whether the router answered "no such route" (the 404 for unknown
/// `/api/v1` paths and wrong methods), as opposed to any handler's answer.
async fn routed(h: &H, method: &Method, path: &str) -> bool {
    let mut b = Request::builder().method(method.clone()).uri(path);
    let body = if *method == Method::GET {
        Body::empty()
    } else {
        b = b.header(header::CONTENT_TYPE, "application/json");
        Body::from("{}")
    };
    let (status, _, bytes) = h.send(b.body(body).unwrap()).await;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    !(status.as_u16() == 404
        && v == json!({"status": "error", "message": "Not found", "path": path}))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_web_endpoint_is_served_or_named_as_not_served() {
    let h = H::new().await;
    let routes = web_routes();
    assert!(routes.len() > 60, "read {} routes", routes.len());
    let mut missing = Vec::new();
    let mut stale = Vec::new();
    for (method, path) in &routes {
        let excused = NOT_SERVED.iter().find(|(p, _)| path.starts_with(p));
        match (routed(&h, method, path).await, excused) {
            (false, None) => missing.push(format!("{} {}", method, path)),
            (true, Some((p, _))) => stale.push(format!("{} {} (exception {})", method, path, p)),
            _ => {}
        }
    }
    assert!(
        missing.is_empty(),
        "web endpoints not served: {:#?}",
        missing
    );
    assert!(
        stale.is_empty(),
        "served now: remove the exception: {:#?}",
        stale
    );
    for (prefix, _) in NOT_SERVED {
        assert!(
            routes.iter().any(|(_, p)| p.starts_with(prefix)),
            "exception {} names no web endpoint",
            prefix
        );
    }
    h.shutdown().await;
}
