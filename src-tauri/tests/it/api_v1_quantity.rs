//! API-02: a whole-number quantity larger than any broker takes (a 32-bit
//! integer) is refused before anything is dispatched, simulated or logged.
//! It used to reach the broker clamped to 2147483647 while the sandbox and
//! the order log kept the quantity sent.

use crate::api_v1_support::H;
use openalgo_desktop_lib::brokers::mock::MockCall;
use serde_json::{json, Value};

const TOO_LARGE: &str = "larger than any exchange accepts";

fn order(quantity: Value) -> Value {
    json!({"strategy": "t", "exchange": "NSE", "symbol": "SBIN", "action": "BUY",
        "quantity": quantity, "pricetype": "MARKET", "product": "MIS"})
}

/// A basket leg: an order without its strategy, which the basket carries.
fn leg(quantity: Value) -> Value {
    let mut leg = order(quantity);
    if let Some(m) = leg.as_object_mut() {
        m.remove("strategy");
    }
    leg
}

fn modify(quantity: Value) -> Value {
    json!({"strategy": "t", "exchange": "NSE", "symbol": "SBIN", "orderid": "1",
        "action": "BUY", "product": "MIS", "pricetype": "LIMIT", "price": 100.0,
        "quantity": quantity, "disclosed_quantity": 0, "trigger_price": 0.0})
}

/// Orders or modifications that reached the mock broker.
fn dispatched(h: &H) -> usize {
    h.mock
        .calls
        .lock()
        .iter()
        .filter(|c| matches!(c, MockCall::PlaceOrder(_) | MockCall::ModifyOrder(_)))
        .count()
}

async fn refused(h: &H, path: &str, body: Value) {
    let (s, v) = h.post(path, h.with_key(body.clone())).await;
    assert_eq!(s.as_u16(), 400, "{} {} -> {}", path, body, v);
    assert!(
        v.to_string().contains(TOO_LARGE),
        "{} {} -> {}",
        path,
        body,
        v
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversize_quantity_is_refused_before_dispatch() {
    let h = H::new().await;
    for analyze in [false, true] {
        h.analyze(analyze);
        for q in [json!(2_147_483_648u64), json!(1e12), json!("2147483648")] {
            refused(&h, "/api/v1/placeorder", order(q.clone())).await;
            refused(&h, "/api/v1/modifyorder", modify(q.clone())).await;
            let basket = json!({"strategy": "t", "orders": [leg(json!(1)), leg(q)]});
            refused(&h, "/api/v1/basketorder", basket).await;
        }
        let mut disclosed = order(json!(10));
        disclosed["disclosed_quantity"] = json!(2_147_483_648u64);
        refused(&h, "/api/v1/placeorder", disclosed).await;
    }
    assert_eq!(dispatched(&h), 0, "nothing reached the broker");
    let (_, book) = h.post("/api/v1/orderbook", h.with_key(json!({}))).await;
    assert_eq!(
        book["data"]["orders"],
        json!([]),
        "nothing reached the sandbox: {}",
        book
    );

    // The largest quantity a broker takes still goes through unchanged.
    h.analyze(false);
    let (s, v) = h
        .post(
            "/api/v1/placeorder",
            h.with_key(order(json!(2_147_483_647u64))),
        )
        .await;
    assert_eq!(s.as_u16(), 200, "{}", v);
    let sent = h.mock.calls.lock().iter().find_map(|c| match c {
        MockCall::PlaceOrder(o) => Some(o.quantity),
        _ => None,
    });
    assert_eq!(sent, Some(2_147_483_647));
    h.shutdown().await;
}
