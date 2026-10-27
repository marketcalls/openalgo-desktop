//! `GET /agent/api/audit` (D-006): the audit trail's read route, its
//! filters, ownership and redaction, over HTTP.

use super::*;
use crate::agent::safety::audit::{self, Entry, Phase};

const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

fn row(h: &H, tool: &str, conversation: i64, run: &str) {
    let mut e = Entry::new(Phase::Attempt, tool);
    e.conversation_id = Some(conversation);
    e.run_id = Some(run.into());
    e.args = Some(json!({"symbol": "SBIN", "apikey": KEY}));
    audit::write(h.ctx(), e).unwrap();
}

fn json_get(path: &str) -> Request<Body> {
    let mut r = get(path);
    r.headers_mut()
        .insert(header::ACCEPT, "application/json".parse().unwrap());
    r
}

#[tokio::test]
async fn the_trail_is_read_newest_first_with_the_webs_filters() {
    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    row(&h, "place_order", 1, "r1");
    row(&h, "cancel_order", 1, "r1");
    row(&h, "place_order", 2, "r2");
    let read = |path: &'static str| {
        let req = with_session(json_get(path), &cookie, None);
        h.json(req)
    };
    let (s, v) = read("/agent/api/audit").await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["status"], "success");
    let tools: Vec<&str> = v["data"].as_array().unwrap().iter().map(|r| r["tool"].as_str().unwrap()).collect();
    assert_eq!(tools, ["place_order", "cancel_order", "place_order"]);
    let first = &v["data"][0];
    for key in ["id", "ts", "phase", "tool", "conversation_id", "run_id", "attempt_id", "args",
        "risk_verdict", "ok", "response", "order_ids"] {
        assert!(first.get(key).is_some(), "{} missing from {}", key, first);
    }
    assert!(!v.to_string().contains(KEY), "a secret was returned: {}", v);
    let (_, v) = read("/agent/api/audit?conversation_id=1&limit=1").await;
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"][0]["tool"], "cancel_order");
    let (_, v) = read("/agent/api/audit?run_id=r2").await;
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    let (_, v) = read("/agent/api/audit?tool=place_order").await;
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    let (s, v) = read("/agent/api/audit?conversation_id=one").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v, json!({"status": "error", "message": "conversation_id must be a whole number"}));
}

#[tokio::test]
async fn another_users_conversation_is_not_found_and_strangers_are_refused() {
    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    let theirs = {
        let c = h.ctx().sqlite.conn().unwrap();
        crate::agent::conversations::store::create_conversation(&c, "someone-else", None, "chat", h.ctx().now())
            .unwrap()
            .id
    };
    let path = format!("/agent/api/audit?conversation_id={}", theirs);
    let (s, v) = h.json(with_session(json_get(&path), &cookie, None)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v, json!({"status": "error", "message": "Conversation not found"}));
    let (s, _) = h.json(json_get("/agent/api/audit")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "the trail is for the signed-in user only");
}
