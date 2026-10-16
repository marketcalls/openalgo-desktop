//! API-01: `/api/v1/chart`, written from the web's source
//! (`restx_api/chart_api.py`, `services/chart_service.py`). The recorded
//! GET fixtures (`tests/fixtures/web/rest/chart/`) replay in
//! `api_v1_contract`; these cover POST, which was not recorded.

use crate::api_v1_support::H;
use axum::body::Body;
use axum::http::{Method, Request};
use serde_json::{json, Value};

async fn get(h: &H, query: &str) -> (u16, Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/v1/chart{}", query))
        .body(Body::empty())
        .unwrap();
    let (s, _, b) = h.send(req).await;
    (
        s.as_u16(),
        serde_json::from_slice(&b).unwrap_or(Value::Null),
    )
}

async fn post(h: &H, body: Value) -> (u16, Value) {
    let (s, v) = h.post("/api/v1/chart", body).await;
    (s.as_u16(), v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_preferences_are_read_back_as_the_web_stores_them() {
    let h = H::new().await;
    let saved = h.with_key(
        json!({"tv_theme": "dark", "tv_chart_layout": "{\"a\":1}", "count": 5,
            "on": true, "gone": null, "nested": {"x": [1, 2]}}),
    );
    assert_eq!(
        post(&h, saved).await,
        (
            200,
            json!({"status": "success", "message": "Preferences updated successfully"})
        )
    );
    // A later save changes only its own keys.
    let changed = h.with_key(json!({"tv_theme": "light"}));
    assert_eq!(post(&h, changed).await.0, 200);
    let (s, v) = get(&h, &format!("?apikey={}", h.key)).await;
    assert_eq!(s, 200, "{}", v);
    assert_eq!(
        v,
        json!({"status": "success", "data": {
            "tv_theme": "light", "tv_chart_layout": "{\"a\":1}", "count": "5", "on": "1",
            "gone": null, "nested": "{\"x\":[1,2]}",
        }})
    );
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_follow_the_web_in_its_order() {
    let h = H::new().await;
    let error = |m: &str| json!({"status": "error", "message": m});
    assert_eq!(post(&h, json!({})).await, (400, error("No data provided")));
    assert_eq!(
        post(&h, json!({"tv_theme": "dark"})).await,
        (
            400,
            json!({"status": "error", "message": {"apikey": ["Missing data for required field."]}})
        )
    );
    assert_eq!(
        post(&h, h.with_key(json!({}))).await,
        (400, error("No preferences provided to update"))
    );
    // The limits are checked before the key, as on the web.
    let mut many = serde_json::Map::new();
    for i in 0..51 {
        many.insert(format!("k{}", i), json!("v"));
    }
    many.insert("apikey".into(), json!("wrong"));
    assert_eq!(
        post(&h, Value::Object(many)).await,
        (400, error("Too many preference keys (max 50)"))
    );
    let mut long = serde_json::Map::new();
    long.insert("apikey".into(), json!("wrong"));
    long.insert("x".repeat(51), json!(1));
    assert_eq!(
        post(&h, Value::Object(long)).await,
        (
            400,
            error("Preference key too long: xxxxxxxxxxxxxxxxxxxx... (max 50 chars)")
        )
    );
    assert_eq!(
        post(&h, json!({"apikey": "wrong", "tv_theme": "dark"})).await,
        (403, error("Invalid openalgo apikey"))
    );
    assert_eq!(
        get(&h, "?apikey=wrong").await,
        (403, error("Invalid openalgo apikey"))
    );
    assert_eq!(get(&h, "").await, (400, error("Missing apikey parameter")));
    h.shutdown().await;
}
