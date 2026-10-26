//! `/api/v1` answers brought to the web's: the instruments CSV's crypto lot
//! (docs-17), history from Historify without a broker session (docs-30),
//! and the analyzer toggle refused in Semi-Auto (docs-32).

use super::*;

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

/// docs-17: the CSV download carries a fractional crypto lot as the JSON
/// answer does (`0.0001`); it wrote the row's whole lot, `1`.
#[tokio::test]
async fn the_instruments_csv_carries_a_fractional_lot() {
    use crate::brokers::common::symbols::tests::row;
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let spot = row("BTCINR", "BTC_INR", "CRYPTO", "16614");
    let mut master = crate::brokers::types::MasterContract::new(vec![spot.clone()]);
    master.lot_sizes.insert(spot.token.clone(), 0.0001);
    {
        let mut c = h.ctx().sqlite.conn().unwrap();
        crate::db::sqlite::symbol::store_master(&mut c, "zerodha", h.ctx().now(), &master).unwrap();
    }
    crate::services::master_contract_service::load_cached(h.ctx(), "zerodha")
        .await
        .unwrap();
    let path = format!("/api/v1/instruments?apikey={}&exchange=CRYPTO", key);
    let (s, v) = h.json(get(&path)).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["data"][0]["lotsize"], json!(0.0001));
    let (s, _, body) = h.send(get(&format!("{}&format=csv", path))).await;
    assert_eq!(s, StatusCode::OK);
    let csv = String::from_utf8(body).unwrap();
    let line = csv.lines().nth(1).unwrap();
    assert_eq!(line.split(',').nth(8), Some("0.0001"), "{}", line);
}

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

/// docs-32: in Semi-Auto the API cannot switch analyzer mode (web 403,
/// SEBI Research Analyst compliance); the mode stays as it was. In Auto it
/// switches as before.
#[tokio::test]
async fn the_analyzer_toggle_is_refused_in_semi_auto() {
    use crate::services::apikey_service::ApiKeyService;
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    ApiKeyService::set_order_mode(h.ctx(), "semi_auto").unwrap();
    let toggle = json!({"apikey": key, "mode": true});
    let (s, v) = h
        .json(post_json("/api/v1/analyzer/toggle", toggle.clone()))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{}", v);
    assert_eq!(
        v,
        json!({"status": "error",
               "message": crate::services::analyzer_service::TOGGLE_SEMI_AUTO})
    );
    assert!(!h.ctx().sqlite.get_analyze_mode().unwrap(), "not switched");
    ApiKeyService::set_order_mode(h.ctx(), "auto").unwrap();
    let (s, v) = h.json(post_json("/api/v1/analyzer/toggle", toggle)).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["data"]["analyze_mode"], true);
}
