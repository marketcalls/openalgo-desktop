//! web: test/test_strategy_module_api.py, test_strategy_module_lifecycle_api.py

use super::*;
use axum::http::{Method, StatusCode};

fn create_body() -> Value {
    json!({
        "name": "Short straddle",
        "underlying": "NIFTY",
        "underlying_exchange": "NSE_INDEX",
        "strategy_type": "positional",
        "legs": [short_call_leg()],
    })
}

#[tokio::test]
async fn create_answers_201_and_shows_the_token_once() {
    let a = app();
    let (s, b) = a.post("/strategy/api/strategies", create_body()).await;
    assert_eq!(s, StatusCode::CREATED, "{}", b);
    assert_eq!(b["status"], "success");
    let token = b["webhook_token"].as_str().unwrap().to_string();
    assert!(token.starts_with("oaws_"));
    assert!(b["message"]
        .as_str()
        .unwrap()
        .contains("Copy the webhook token now"));
    let sid = b["data"]["id"].as_i64().unwrap();
    let (s, d) = a.get(&format!("/strategy/api/strategies/{}", sid)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!d.to_string().contains(&token));
    let (_, list) = a.get("/strategy/api/strategies").await;
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn every_session_route_needs_a_signed_in_user() {
    let a = app();
    let mut r = a.req(Method::GET, "/strategy/api/strategies", None, false);
    r.headers_mut().remove(axum::http::header::COOKIE);
    let (s, b) = a.send(r).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(b["message"], "Not authenticated");
}

#[tokio::test]
async fn a_write_without_the_csrf_token_is_refused() {
    let a = app();
    let (s, _) = a
        .send(a.req(
            Method::POST,
            "/strategy/api/strategies",
            Some(create_body()),
            false,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "CSRF refusal");
    let mut r = a.req(
        Method::POST,
        "/strategy/api/strategies",
        Some(create_body()),
        true,
    );
    r.headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let (s, _) = a.send(r).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_strategy_that_is_not_yours_answers_404_not_403() {
    let a = app();
    let (row, _) = a
        .ctx
        .strategy
        .store
        .create_strategy(
            "someone-else",
            &config("Theirs", json!([short_call_leg()]), json!({})),
        )
        .unwrap();
    for path in ["", "/runs", "/orders", "/events", "/checkpoints"] {
        let (s, b) = a
            .get(&format!("/strategy/api/strategies/{}{}", row.id, path))
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{}", path);
        assert_eq!(b["message"], "Strategy not found");
    }
    let (s, _) = a.get("/strategy/api/strategies/999999").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn validation_errors_are_400_with_a_message() {
    let a = app();
    let mut body = create_body();
    body["legs"] = json!([]);
    let (s, b) = a.post("/strategy/api/strategies", body).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        b,
        json!({"status": "error", "message": "A strategy needs at least 1 leg"})
    );
}

#[tokio::test]
async fn a_patch_revalidates_the_merged_config_and_refuses_a_kind_change() {
    let a = app();
    let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
    let sid = b["data"]["id"].as_i64().unwrap();
    let path = format!("/strategy/api/strategies/{}", sid);
    let (s, b) = a
        .send(a.req(
            Method::PATCH,
            &path,
            Some(json!({"overall_sl_mtm": 2500})),
            true,
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(b["data"]["overall_sl_mtm"], 2500.0);
    let (s, b) = a
        .send(a.req(
            Method::PATCH,
            &path,
            Some(json!({"strategy_kind": "signal"})),
            true,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(b["message"]
        .as_str()
        .unwrap()
        .contains("cannot change between batch and signal"));
    let (s, _) = a
        .send(a.req(Method::PATCH, &path, Some(json!({})), true))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, b) = a
        .send(a.req(
            Method::PATCH,
            &path,
            Some(json!({"strategy_type": "intraday"})),
            true,
        ))
        .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "merged config re-validated: {}",
        b
    );
}

#[tokio::test]
async fn start_requires_a_mode() {
    let a = app();
    let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
    let sid = b["data"]["id"].as_i64().unwrap();
    let (s, b) = a
        .post(
            &format!("/strategy/api/strategies/{}/start", sid),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(b["message"], "mode must be one of: live, sandbox");
    let (s, _) = a
        .post(&format!("/strategy/api/strategies/{}/stop", sid), json!({}))
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_sandbox_run_starts_fills_through_the_sandbox_and_stops_flat() {
    let a = app();
    let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
    let sid = b["data"]["id"].as_i64().unwrap();
    let (s, b) = a
        .post(
            &format!("/strategy/api/strategies/{}/start", sid),
            json!({"mode": "sandbox"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    let run = b["run_id"].as_i64().unwrap();
    assert_eq!(b["mode"], "sandbox");
    // The sandbox fills the MARKET entry and publishes order.update; the
    // strategy subscriber folds it into the leg.
    let m = a.ctx.strategy.clone();
    assert!(
        a.until(|| m
            .state
            .snapshot(run)
            .map(|s| s.legs["1"].entry_status == "complete")
            .unwrap_or(false))
            .await
    );
    assert_eq!(m.state.snapshot(run).unwrap().legs["1"].entry_avg, 100.0);
    let (s, b) = a
        .post(&format!("/strategy/api/strategies/{}/stop", sid), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    let store = a.ctx.strategy.store.clone();
    assert!(
        a.until(|| store.get_run(run).unwrap().unwrap().stopped_at.is_some())
            .await
    );
    let (_, orders) = a
        .get(&format!("/strategy/api/strategies/{}/orders", sid))
        .await;
    let rows = orders["data"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["action"], "BUY");
    assert_eq!(rows[1]["status"], "complete");
    // Nothing reached the live broker.
    assert!(!a.mock.calls().iter().any(|c| matches!(
        c,
        openalgo_desktop_lib::brokers::mock::MockCall::PlaceOrder(_)
    )));
    // The orderbook view reads the sandbox book for a sandbox run.
    let (s, ob) = a
        .get(&format!("/strategy/api/strategies/{}/orderbook", sid))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", ob);
    assert_eq!(ob["data"]["orders"].as_array().unwrap().len(), 2);
    assert_eq!(ob["mode"], "analyze");
    let (_, cps) = a
        .get(&format!("/strategy/api/strategies/{}/checkpoints", sid))
        .await;
    assert_eq!(cps["run_id"], run);
    let (_, runs) = a
        .get(&format!("/strategy/api/strategies/{}/runs", sid))
        .await;
    assert_eq!(runs["data"][0]["stop_reason"], "manual");
}

#[tokio::test]
async fn the_kill_switch_locks_and_flattens() {
    let a = app();
    let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
    let sid = b["data"]["id"].as_i64().unwrap();
    let (s, b) = a
        .post(
            &format!("/strategy/api/strategies/{}/kill_switch", sid),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["webhook_locked"], true);
    assert_eq!(b["run_stopped"], false);
    let (s, b) = a
        .post(
            &format!("/strategy/api/strategies/{}/unlock_webhook", sid),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, b["webhook_locked"].clone()),
        (StatusCode::OK, json!(false))
    );
}

#[tokio::test]
async fn rotate_returns_a_new_token_and_live_toggles() {
    let a = app();
    let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
    let sid = b["data"]["id"].as_i64().unwrap();
    let old = b["webhook_token"].as_str().unwrap().to_string();
    let (s, b) = a
        .post(
            &format!("/strategy/api/strategies/{}/webhook/rotate", sid),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_ne!(b["webhook_token"].as_str().unwrap(), old);
    let (s, _) = a
        .post(&format!("/strategy/api/strategies/{}/live", sid), json!({}))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, b) = a
        .post(
            &format!("/strategy/api/strategies/{}/live", sid),
            json!({"enabled": true}),
        )
        .await;
    assert_eq!(
        (s, b["live_enabled"].clone()),
        (StatusCode::OK, json!(true))
    );
}

#[tokio::test]
async fn the_events_query_is_validated_and_clamped() {
    let a = app();
    let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
    let sid = b["data"]["id"].as_i64().unwrap();
    let (s, _) = a
        .get(&format!(
            "/strategy/api/strategies/{}/events?kind=bogus",
            sid
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = a
        .get(&format!("/strategy/api/strategies/{}/events?limit=x", sid))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, b) = a
        .get(&format!("/strategy/api/strategies/{}/events?limit=-1", sid))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"].as_array().unwrap().len(), 1, "clamped to 1");
}

#[tokio::test]
async fn the_public_webhook_needs_no_session_or_csrf() {
    let a = app();
    let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
    let token = b["webhook_token"].as_str().unwrap().to_string();
    let (s, b) = a
        .webhook(&token, r#"{"action":"start","mode":"sandbox"}"#)
        .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(b["result"], "ok");
    let (s, b) = a.webhook("oaws_unknown_but_long_enough_xx", "{}").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        b,
        json!({"status": "error", "result": "rejected_token", "message": "Unknown or expired webhook token"})
    );
    let big = format!("{{\"x\":\"{}\"}}", "a".repeat(17000));
    let (s, _) = a.webhook(&token, &big).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}
