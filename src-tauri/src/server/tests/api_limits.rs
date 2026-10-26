//! `/api/v1` rate limits as the web sets them: each endpoint in its own
//! window per caller (Flask-Limiter scopes a route's limit by its
//! endpoint), at the limit the web's decorator gives it. The limiter's
//! clock is pinned by `H::new`, so a window cannot slide while a request
//! is still being sent.

use super::*;

fn place(key: &str) -> Value {
    json!({
        "apikey": key, "strategy": "t", "exchange": "NSE", "symbol": "SBIN",
        "action": "BUY", "quantity": 1, "pricetype": "MARKET", "product": "MIS",
    })
}

/// docs-19: ten places and ten cancels in the same second all reach their
/// handlers (the desktop counted them in one shared order window and
/// refused the cancels); the eleventh place gets the web's 429.
#[tokio::test]
async fn order_endpoints_count_in_their_own_windows() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let cancel = json!({"apikey": key, "strategy": "t", "orderid": "1"});
    for _ in 0..10 {
        let (s, v) = h.json(post_json("/api/v1/placeorder", place(&key))).await;
        assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "place: {}", v);
        let (s, v) = h
            .json(post_json("/api/v1/cancelorder", cancel.clone()))
            .await;
        assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "cancel: {}", v);
    }
    let (s, v) = h.json(post_json("/api/v1/placeorder", place(&key))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v, json!({"message": "10 per 1 second"}));
}

/// docs-1: optiongreeks (`GREEKS_RATE_LIMIT`) and whatsapp/notify
/// (`WHATSAPP_RATE_LIMIT`) allow 30 a minute, each in its own window; the
/// desktop counted both at the general 100 a second.
#[tokio::test]
async fn greeks_and_whatsapp_allow_30_a_minute() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    for path in ["/api/v1/optiongreeks", "/api/v1/whatsapp/notify"] {
        for _ in 0..30 {
            let (s, v) = h.json(post_json(path, json!({"apikey": key}))).await;
            assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "{}: {}", path, v);
        }
        let (s, v) = h.json(post_json(path, json!({"apikey": key}))).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", path);
        assert_eq!(v, json!({"message": "30 per 1 minute"}), "{}", path);
    }
}
