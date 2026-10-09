//! The public webhook can place real orders: per-address limits, per-webhook
//! lockout, nothing throttled or locked reaches the order path.

use super::*;
use axum::http::StatusCode;
use openalgo_desktop_lib::strategy::webhook::LOCKOUT_FAILURES;

const START: &str = r#"{"action":"start","mode":"sandbox"}"#;

async fn create(a: &App, allowlist: Value) -> (i64, String) {
    let (s, b) = a
        .post(
            "/strategy/api/strategies",
            json!({
                "name": "Guarded",
                "underlying": "NIFTY",
                "underlying_exchange": "NSE_INDEX",
                "strategy_type": "positional",
                "legs": [short_call_leg()],
                "webhook_ip_allowlist": allowlist,
            }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", b);
    (
        b["data"]["id"].as_i64().unwrap(),
        b["webhook_token"].as_str().unwrap().to_string(),
    )
}

fn audit_rows(a: &App, sid: i64) -> usize {
    a.ctx
        .strategy
        .store
        .list_webhook_events(sid, 1000)
        .unwrap()
        .len()
}

async fn orders(a: &App, sid: i64) -> usize {
    let (_, b) = a
        .get(&format!("/strategy/api/strategies/{}/orders", sid))
        .await;
    b["data"].as_array().map(|v| v.len()).unwrap_or(0)
}

fn locked(a: &App, sid: i64) -> bool {
    a.ctx
        .strategy
        .store
        .get_strategy(sid, USER)
        .unwrap()
        .unwrap()
        .webhook_locked
}

#[tokio::test]
async fn a_burst_over_the_limit_is_refused_before_any_check_or_order() {
    let a = app();
    let (sid, token) = create(&a, json!([])).await;
    // 100 per minute (web WEBHOOK_RATE_LIMIT); each reaches the pipeline
    // and is refused there for its payload, so no order is placed.
    for _ in 0..100 {
        let (s, b) = a.webhook_from("198.51.100.1", &token, "{}").await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", b);
    }
    assert_eq!(audit_rows(&a, sid), 100);
    // Over the limit: refused before the token is even looked up, even
    // for a well-formed start alert.
    let (s, b) = a.webhook_from("198.51.100.1", &token, START).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(b["result"], "rate_limited");
    let (s, _) = a.webhook_from("198.51.100.1", "not-a-token", START).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(audit_rows(&a, sid), 100, "no audit row, no lookup");
    assert_eq!(orders(&a, sid).await, 0);
    assert!(a.ctx.strategy.store.list_runs(sid, 10).unwrap().is_empty());
}

#[tokio::test]
async fn bad_attempts_lock_the_webhook_until_the_trader_unlocks_it() {
    let a = app();
    let (sid, token) = create(&a, json!(["10.0.0.0/8"])).await;
    for i in 0..LOCKOUT_FAILURES {
        let (s, b) = a.webhook_from("203.0.113.9", &token, START).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "attempt {}: {}", i, b);
        assert_eq!(b["result"], "rejected_ip");
    }
    assert!(locked(&a, sid));
    // The right caller with the right token is refused while locked.
    let (s, b) = a.webhook_from("10.0.0.7", &token, START).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{}", b);
    assert_eq!(b["result"], "rejected_locked");
    assert_eq!(orders(&a, sid).await, 0);
    let (_, ev) = a
        .get(&format!("/strategy/api/strategies/{}/events", sid))
        .await;
    assert!(
        ev.to_string().contains("webhook_locked"),
        "a critical event tells the trader: {}",
        ev
    );

    // Unlock restores it.
    let (s, b) = a
        .post(
            &format!("/strategy/api/strategies/{}/unlock_webhook", sid),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert!(!locked(&a, sid));
    let (s, b) = a.webhook_from("10.0.0.7", &token, START).await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(b["result"], "ok");
}

#[tokio::test]
async fn rotating_source_addresses_still_trips_the_per_webhook_lockout() {
    let a = app();
    let (sid, token) = create(&a, json!(["10.0.0.0/8"])).await;
    for i in 0..LOCKOUT_FAILURES {
        let ip = format!("192.0.2.{}", i + 1);
        let (s, _) = a.webhook_from(&ip, &token, START).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    assert!(locked(&a, sid));
    let (s, b) = a.webhook_from("10.1.2.3", &token, START).await;
    assert_eq!(b["result"], "rejected_locked", "{}", b);
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(orders(&a, sid).await, 0);
}

/// Ten wrong tokens from one address spend its failure budget: its further
/// guesses are refused at once, with no lookup and no audit row. The real
/// token from that address still works (a valid token is never refused
/// because of failures), and another address is unaffected.
#[tokio::test]
async fn guessing_tokens_from_one_address_spends_only_its_failure_budget() {
    let a = app();
    let (sid, token) = create(&a, json!([])).await;
    for i in 0..10 {
        let guess = format!("oaws_{:0>43}", i);
        let (s, _) = a.webhook_from("198.51.100.77", &guess, "{}").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    let unattributed = a
        .ctx
        .strategy
        .store
        .count_unattributed_webhook_events()
        .unwrap();
    let (s, b) = a
        .webhook_from("198.51.100.77", &format!("oaws_{:0>43}", 99), "{}")
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", b);
    assert_eq!(b["result"], "rate_limited");
    assert_eq!(
        a.ctx
            .strategy
            .store
            .count_unattributed_webhook_events()
            .unwrap(),
        unattributed,
        "no audit row once the budget is spent"
    );
    let (s, b) = a.webhook_from("198.51.100.77", &token, START).await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(audit_rows(&a, sid), 1);
    let (s, _) = a
        .webhook_from("198.51.100.78", &format!("oaws_{:0>43}", 98), "{}")
        .await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "another address has its own budget"
    );
}

#[tokio::test]
async fn a_valid_alert_within_limits_places_exactly_one_order() {
    let a = app();
    let (sid, token) = create(&a, json!(["10.0.0.0/8"])).await;
    let (s, b) = a.webhook_from("10.0.0.7", &token, START).await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(b["result"], "ok");
    let store = a.ctx.strategy.store.clone();
    assert!(
        a.until(|| store
            .list_orders_for_strategy(sid, None)
            .map(|v| !v.is_empty())
            .unwrap_or(false))
            .await
    );
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(orders(&a, sid).await, 1);
    assert!(!locked(&a, sid));
}
