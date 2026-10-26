//! docs-20, docs-29: a request the broker's pacer refused before sending
//! answers 429 with its sentence, as the web's `BrokerBusyError` does
//! (`services/broker_busy.py`, the order services, `restx_api/ticker.py`).
//! The desktop answered 500 for market data and 400 for orders.

use super::*;
use crate::brokers::common::ratelimit::{Pacer, BUSY_MESSAGE};
use crate::brokers::common::symbols::tests::row;
use std::time::Duration;

/// SBIN on NSE in the master, and a mock broker whose pacer has its next
/// turn a minute away, past the ten-second bound: every broker call is
/// refused before it is sent.
async fn busy_broker(h: &H) {
    let master =
        crate::brokers::types::MasterContract::new(vec![row("SBIN", "SBIN-EQ", "NSE", "3045")]);
    {
        let mut c = h.ctx().sqlite.conn().unwrap();
        crate::db::sqlite::symbol::store_master(&mut c, "zerodha", h.ctx().now(), &master).unwrap();
    }
    crate::services::master_contract_service::load_cached(h.ctx(), "zerodha")
        .await
        .unwrap();
    h.mock
        .quotes
        .lock()
        .insert("NSE:SBIN".into(), crate::brokers::types::Quote::default());
    let pacer = Arc::new(Pacer::with_interval(Duration::from_secs(60)));
    pacer.acquire().await.unwrap();
    *h.mock.pacer.lock() = Some(pacer);
}

fn busy() -> Value {
    json!({"status": "error", "message": BUSY_MESSAGE})
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn market_data_refused_by_the_pacer_answers_429() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    busy_broker(&h).await;
    let sbin = json!({"apikey": key, "symbol": "SBIN", "exchange": "NSE"});
    let history = json!({"apikey": key, "symbol": "SBIN", "exchange": "NSE", "interval": "D",
        "start_date": "2026-10-01", "end_date": "2026-10-05"});
    let multi = json!({"apikey": key, "symbols": [{"symbol": "SBIN", "exchange": "NSE"}]});
    for (path, body) in [
        ("/api/v1/quotes", sbin.clone()),
        ("/api/v1/depth", sbin),
        ("/api/v1/multiquotes", multi),
        ("/api/v1/history", history),
    ] {
        let (s, v) = h.json(post_json(path, body)).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}: {}", path, v);
        assert_eq!(v, busy(), "{}", path);
    }
    let ticker = format!(
        "/api/v1/ticker/NSE:SBIN?apikey={}&interval=D&from=2026-10-01&to=2026-10-05",
        key
    );
    let (s, v) = h.json(get(&ticker)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert_eq!(v, busy());
    // The text form says the same, as the web's ticker does.
    let (s, _, text) = h.send(get(&format!("{}&format=txt", ticker))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        String::from_utf8(text).unwrap(),
        format!("{}\n", BUSY_MESSAGE)
    );
}

/// An order the pacer refused was never sent: 429 with the sentence, and
/// no placement or cancel reached the broker.
#[tokio::test]
async fn an_order_refused_by_the_pacer_answers_429() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    busy_broker(&h).await;
    let place = json!({
        "apikey": key, "strategy": "t", "exchange": "NSE", "symbol": "SBIN",
        "action": "BUY", "quantity": 1, "pricetype": "LIMIT", "price": 100, "product": "MIS",
    });
    let (s, v) = h.json(post_json("/api/v1/placeorder", place)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert_eq!(v, busy());
    let cancel = json!({"apikey": key, "strategy": "t", "orderid": "1"});
    let (s, v) = h.json(post_json("/api/v1/cancelorder", cancel)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert_eq!(v, busy());
    let sent = h.mock.calls.lock().iter().any(|c| {
        matches!(
            c,
            crate::brokers::mock::MockCall::PlaceOrder(_)
                | crate::brokers::mock::MockCall::CancelOrder(_)
        )
    });
    assert!(!sent, "nothing reached the broker");
}
