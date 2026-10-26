//! `/api/v1` answers brought to the web's: history from Historify
//! without a broker session (docs-30).

use super::*;

/// docs-30: `source: "db"` reads Historify on this computer and needs a
/// valid key only, not a broker session (it answered 403 `Invalid openalgo
/// apikey` before the trader logged in to the broker). The broker source
/// still needs the session, and a wrong key is refused either way.
#[tokio::test]
async fn history_from_historify_needs_no_broker_session() {
    let h = H::new();
    let key = h.setup();
    let body = |source: &str, key: &str| {
        json!({"apikey": key, "symbol": "SBIN", "exchange": "NSE", "interval": "D",
               "start_date": "2026-10-01", "end_date": "2026-10-05", "source": source})
    };
    let (s, v) = h.json(post_json("/api/v1/history", body("db", &key))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{}", v);
    assert_eq!(
        v["message"],
        "No data found for SBIN:NSE interval D in local database. Download data first using Historify."
    );
    let invalid = json!({"status": "error", "message": "Invalid openalgo apikey"});
    let (s, v) = h
        .json(post_json("/api/v1/history", body("api", &key)))
        .await;
    assert_eq!((s, v), (StatusCode::FORBIDDEN, invalid.clone()));
    let (s, v) = h
        .json(post_json("/api/v1/history", body("db", "not-the-key")))
        .await;
    assert_eq!((s, v), (StatusCode::FORBIDDEN, invalid));
}
