//! `/api/v1/market/holidays` and `/api/v1/market/timings` against the golden
//! fixtures recorded from OpenAlgo web (`tests/fixtures/web/rest/market`),
//! served from the seeded market calendar tables.

use crate::webui_support;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use webui_support::*;

/// Replay a fixture with the real key; the body must equal the recording.
async fn replay(h: &H, key: &str, rel: &str) {
    let f = fixture(rel);
    let mut body = f["request"]["body"].clone();
    body["apikey"] = json!(key);
    let path = f["request"]["path"].as_str().unwrap();
    let (s, v) = h.json(req(Method::POST, path, Some(body))).await;
    assert_eq!(
        s.as_u16() as u64,
        f["response"]["status_code"].as_u64().unwrap(),
        "{}",
        rel
    );
    assert_eq!(v, f["response"]["body"], "{}", rel);
}

#[tokio::test]
async fn holidays_and_timings_match_the_web_fixtures() {
    // The fixtures were recorded on 2026-10-03 IST.
    let h = H::at(ist(2026, 10, 3, 14, 0));
    let key = h.setup();
    for rel in [
        "market/holidays/year_2026.json",
        "market/holidays/no_year.json",
        "market/holidays/year_out_of_range.json",
        "market/timings/weekday_2026-10-01.json",
        "market/timings/saturday_2026-10-03.json",
        "market/timings/bad_date.json",
    ] {
        replay(&h, &key, rel).await;
    }
}

#[tokio::test]
async fn validation_and_key_errors() {
    let h = H::new();
    let key = h.setup();
    let post = |path: &str, b: Value| req(Method::POST, path, Some(b));
    let cases = [
        (
            "/api/v1/market/holidays",
            json!({}),
            json!({"status": "error", "message": {"apikey": ["Missing data for required field."]}}),
        ),
        (
            "/api/v1/market/holidays",
            json!({"apikey": key, "year": "abc"}),
            json!({"status": "error", "message": {"year": ["Not a valid integer."]}}),
        ),
        (
            "/api/v1/market/holidays",
            json!({"apikey": key, "foo": 1}),
            json!({"status": "error", "message": {"foo": ["Unknown field."]}}),
        ),
        (
            "/api/v1/market/timings",
            json!({"apikey": key}),
            json!({"status": "error", "message": {"date": ["Missing data for required field."]}}),
        ),
        (
            "/api/v1/market/timings",
            json!({"apikey": key, "date": 20261001}),
            json!({"status": "error", "message": {"date": ["Not a valid string."]}}),
        ),
        (
            "/api/v1/market/timings",
            json!({"apikey": key, "date": "2019-12-31"}),
            json!({"status": "error", "message": "Date must be between 2020-01-01 and 2050-12-31"}),
        ),
    ];
    for (p, b, want) in cases {
        let (s, v) = h.json(post(p, b)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", p);
        assert_eq!(v, want);
    }
    for (p, b) in [
        ("/api/v1/market/holidays", json!({"apikey": "wrongkey"})),
        (
            "/api/v1/market/timings",
            json!({"apikey": "wrongkey", "date": "2026-10-01"}),
        ),
    ] {
        let (s, v) = h.json(post(p, b)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(
            v,
            json!({"status": "error", "message": "Invalid openalgo apikey"})
        );
        // Wrong method is 404, as on the web.
        let (s, _) = h.json(get(p)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    // A valid key works without a broker session (calendar data only).
    let (s, v) = h
        .json(post(
            "/api/v1/market/timings",
            json!({"apikey": key, "date": "2026-11-08"}),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let w = v["data"].as_array().unwrap();
    assert!(
        w.iter().any(|x| x["exchange"] == "NSE"),
        "Muhurat special session"
    );
}

#[tokio::test]
async fn admin_holiday_changes_reach_the_api() {
    let (h, c, t) = signed_in();
    let key = openalgo_desktop_lib::services::apikey_service::ApiKeyService::current(&h.ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    let (s, _) = h
        .json(with(
            req(Method::POST, "/admin/api/holidays", Some(json!({
                "date": "2026-10-06", "description": "Special close", "holiday_type": "TRADING_HOLIDAY",
                "closed_exchanges": ["NSE", "BSE", "NFO", "BFO", "CDS", "BCD"],
                "open_exchanges": [{"exchange": "MCX", "start_time": 1791198000000_i64, "end_time": 1791222900000_i64}],
            }))),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = h
        .json(req(
            Method::POST,
            "/api/v1/market/timings",
            Some(json!({"apikey": key, "date": "2026-10-06"})),
        ))
        .await;
    assert_eq!(
        v,
        json!({"status": "success", "data": [{"exchange": "MCX", "start_time": 1791198000000_i64, "end_time": 1791222900000_i64}]})
    );
    let (_, v) = h
        .json(req(
            Method::POST,
            "/api/v1/market/holidays",
            Some(json!({"apikey": key, "year": 2026})),
        ))
        .await;
    assert_eq!(v["data"].as_array().unwrap().len(), 18);
}
