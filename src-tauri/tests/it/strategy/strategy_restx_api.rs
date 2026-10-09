//! web: test/test_strategy_restx_api.py

use super::*;
use axum::http::StatusCode;

async fn made(a: &App) -> i64 {
    let (_, b) = a
        .post(
            "/strategy/api/strategies",
            json!({"name": "API", "underlying": "NIFTY", "underlying_exchange": "NSE_INDEX",
                   "strategy_type": "positional", "legs": [short_call_leg()]}),
        )
        .await;
    b["data"]["id"].as_i64().unwrap()
}

#[tokio::test]
async fn mode_is_required_on_start_and_never_defaulted() {
    let a = app();
    let sid = made(&a).await;
    let (s, b) = a
        .api("/api/v1/strategy/start", json!({"strategy_id": sid}))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(b["message"]["mode"][0], "Missing data for required field.");
    let (s, b) = a
        .api(
            "/api/v1/strategy/start",
            json!({"strategy_id": sid, "mode": "paper"}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(b["message"]["mode"][0], "Must be one of: live, sandbox.");
}

#[tokio::test]
async fn live_without_the_opt_in_is_a_409() {
    let a = app();
    let sid = made(&a).await;
    let (s, b) = a
        .api(
            "/api/v1/strategy/start",
            json!({"strategy_id": sid, "mode": "live"}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(b["message"]
        .as_str()
        .unwrap()
        .contains("not enabled for live trading"));
}

#[tokio::test]
async fn an_invalid_key_is_403_and_a_missing_key_is_400() {
    let a = app();
    let (s, b) = a
        .send(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/strategy/list")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"apikey": "wrong"}).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(
        (s, b["message"].clone()),
        (StatusCode::FORBIDDEN, json!("Invalid openalgo apikey"))
    );
    let (s, b) = a
        .send(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/strategy/list")
                .header("content-type", "application/json")
                .body(axum::body::Body::from("not json"))
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        b["message"]["apikey"][0],
        "Missing data for required field."
    );
}

#[tokio::test]
async fn list_status_runs_orders_and_events_have_the_web_shapes() {
    let a = app();
    let sid = made(&a).await;
    let (s, b) = a.api("/api/v1/strategy/list", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"][0]["id"], sid);
    let (_, b) = a
        .api("/api/v1/strategy/status", json!({"strategy_id": sid}))
        .await;
    assert_eq!(b["status"], "success");
    assert_eq!(b["run"], Value::Null);
    assert!(b["data"]["legs"].is_array());
    assert!(!b.to_string().contains("oaws_"));
    let (s, b) = a
        .api(
            "/api/v1/strategy/start",
            json!({"strategy_id": sid, "mode": "sandbox"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    let run = b["run_id"].as_i64().unwrap();
    let (_, b) = a
        .api(
            "/api/v1/strategy/runs",
            json!({"strategy_id": sid, "limit": 5}),
        )
        .await;
    assert_eq!(b["data"][0]["id"], run);
    let (_, b) = a
        .api(
            "/api/v1/strategy/orders",
            json!({"strategy_id": sid, "run_id": run}),
        )
        .await;
    assert_eq!(b["data"][0]["kind"], "entry");
    let (s, b) = a
        .api(
            "/api/v1/strategy/events",
            json!({"strategy_id": sid, "limit": 0}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        b["message"]["limit"][0],
        "Must be greater than or equal to 1 and less than or equal to 1000."
    );
    let (_, b) = a
        .api(
            "/api/v1/strategy/events",
            json!({"strategy_id": sid, "kind": "run_started"}),
        )
        .await;
    assert_eq!(b["data"].as_array().unwrap().len(), 1);
    // The sandbox entry fills on the engine's own task, so the close is
    // retried while the entry is still unfilled (the 409 tells the
    // caller to retry). Then a second close finds nothing open.
    let mut closed = None;
    for _ in 0..200 {
        let (s, b) = a
            .api(
                "/api/v1/strategy/close_leg",
                json!({"strategy_id": sid, "leg_id": 1}),
            )
            .await;
        if s != StatusCode::CONFLICT {
            closed = Some((s, b));
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let (s, b) = closed.expect("the sandbox entry never filled");
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(
        (b["run_id"].as_i64(), b["leg_id"].as_i64()),
        (Some(run), Some(1))
    );
    let (s, _) = a
        .api(
            "/api/v1/strategy/close_leg",
            json!({"strategy_id": sid, "leg_id": 1}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "already closed");
}

#[tokio::test]
async fn another_users_strategy_is_404() {
    let a = app();
    let (row, _) = a
        .ctx
        .strategy
        .store
        .create_strategy(
            "someone-else",
            &config("X", json!([short_call_leg()]), json!({})),
        )
        .unwrap();
    let (s, b) = a
        .api("/api/v1/strategy/status", json!({"strategy_id": row.id}))
        .await;
    assert_eq!(
        (s, b["message"].clone()),
        (StatusCode::NOT_FOUND, json!("Strategy not found"))
    );
    let (s, _) = a
        .api("/api/v1/strategy/stop", json!({"strategy_id": row.id}))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stop_and_close_all_on_a_stopped_strategy_are_409() {
    let a = app();
    let sid = made(&a).await;
    for (p, body) in [
        ("stop", json!({"strategy_id": sid})),
        ("close_all", json!({"strategy_id": sid})),
        ("close_leg", json!({"strategy_id": sid, "leg_id": 1})),
    ] {
        let (s, b) = a.api(&format!("/api/v1/strategy/{}", p), body).await;
        assert_eq!(s, StatusCode::CONFLICT, "{}", p);
        assert_eq!(b["message"], "This strategy is not running");
    }
}
