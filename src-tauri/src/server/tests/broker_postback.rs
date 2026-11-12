//! Web `test/test_postback_sender_check.py` (76ebb8593): a broker postback
//! is accepted only when its payload proves who sent it. On the web
//! `POST /postback/<broker>` is public, and for Dhan, Fyers, Upstox, Angel
//! and AliceBlue anyone who could reach the server could post a fill that
//! never happened; it became a live order update that strategies and Flow
//! act on.
//!
//! The desktop serves no postback route at all: order updates come from
//! the broker feeds the app opens itself with the broker token, from order
//! book polling and from the sandbox (`docs/api/websocket-streaming/order-updates.md`,
//! Desktop Differences). These tests pin that for the web's six receivers,
//! Zerodha's signed postback included: refused whichever broker is logged
//! in, the same answer either way, and no order update on the bus.

use crate::brokers::families::noren::auth::sha256_hex;
use crate::events::{Event, Lane, OrderUpdate, Subscriber, Topic};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};

use super::*;

const BROKERS: [&str; 6] = ["zerodha", "dhan", "fyers", "upstox", "angel", "aliceblue"];

/// A filled order as each broker's postback names its fields (the web
/// test's `FORGED`); Zerodha's carries the checksum its postbacks are
/// signed with, made with the saved API secret.
fn forged_fill(broker: &str) -> Value {
    match broker {
        "zerodha" => {
            let (order_id, stamp) = ("240101000001", "2026-01-01 09:15:00");
            let checksum = sha256_hex(&[order_id, stamp, "kitesecret"]);
            json!({"order_id": order_id, "order_timestamp": stamp, "checksum": checksum,
                "status": "COMPLETE", "tradingsymbol": "SBIN", "exchange": "NSE",
                "transaction_type": "BUY", "quantity": 10, "filled_quantity": 10,
                "average_price": 800.5})
        }
        "dhan" => json!({"orderId": "2", "orderStatus": "TRADED", "tradingSymbol": "SBIN",
            "exchangeSegment": "NSE_EQ", "transactionType": "BUY", "quantity": 1,
            "filled_qty": 1, "orderType": "MARKET", "productType": "CNC"}),
        "fyers" => json!({"id": "3", "status": 2, "symbol": "NSE:SBIN-EQ", "qty": 1,
            "filledQty": 1, "side": 1, "type": 2, "productType": "CNC",
            "exchange": 10, "segment": 10}),
        "upstox" => json!({"update_type": "order", "order_id": "4", "status": "complete",
            "trading_symbol": "SBIN-EQ", "exchange": "NSE", "transaction_type": "BUY",
            "quantity": 1, "filled_quantity": 1, "order_type": "MARKET", "product": "D"}),
        "angel" => json!({"orderid": "5", "orderstatus": "complete",
            "tradingsymbol": "SBIN-EQ", "exchange": "NSE", "transactiontype": "BUY",
            "quantity": "1", "filledshares": "1", "unfilledshares": "0",
            "ordertype": "MARKET", "producttype": "DELIVERY"}),
        _ => json!({"norenordno": "6", "status": "Complete", "tsym": "SBIN-EQ",
            "exch": "NSE", "trantype": "B", "qty": 1, "fillshares": 1,
            "prctyp": "MKT", "pcode": "CNC"}),
    }
}

/// Every `order.update` the bus delivers.
#[derive(Default)]
struct OrderUpdates(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl Subscriber for OrderUpdates {
    fn name(&self) -> &'static str {
        "postback-test-order-updates"
    }
    fn topics(&self) -> Vec<Topic> {
        vec![Topic::OrderUpdate]
    }
    async fn handle(&self, event: Arc<Event>) {
        if let Event::OrderUpdate(u) = &*event {
            self.0.lock().push(u.orderid.clone());
        }
    }
}

/// The order ids of the updates delivered so far, read once a marker
/// published now has arrived: a subscriber's queue keeps its order, so
/// anything published before the marker has arrived too.
async fn delivered(h: &H, seen: &OrderUpdates) -> Vec<String> {
    h.ctx().bus.publish(Event::OrderUpdate(OrderUpdate {
        mode: "analyze".into(),
        orderid: "marker".into(),
        ..Default::default()
    }));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !seen.0.lock().iter().any(|id| id == "marker") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the marker never arrived"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let mut ids = std::mem::take(&mut *seen.0.lock());
    ids.retain(|id| id != "marker");
    ids
}

/// Web `test_a_forged_fill_for_the_logged_in_broker_publishes_nothing`,
/// `test_every_broker_without_a_signature_is_refused`,
/// `test_the_answer_does_not_tell_which_broker_is_logged_in` and
/// `test_a_signed_zerodha_postback_is_published_as_before`: on the desktop
/// none is accepted, Zerodha's signed one included, and the answer is the
/// same whether a broker is logged in or not.
#[tokio::test]
async fn a_broker_postback_is_never_accepted() {
    let h = H::new();
    h.setup();
    h.save_broker_credentials();
    let seen = Arc::new(OrderUpdates::default());
    h.ctx().bus.subscribe(seen.clone(), Lane::BestEffort);
    let (cookie, csrf) = h.session(true);
    let mut answers: HashMap<&str, HashSet<(StatusCode, Vec<u8>)>> = HashMap::new();
    for logged_in in [false, true] {
        if logged_in {
            h.connect_broker();
        }
        for broker in BROKERS {
            let path = format!("/postback/{}", broker);
            // As a broker posts it: no browser session.
            let (status, _, body) = h.send(post_json(&path, forged_fill(broker))).await;
            assert!(!status.is_success(), "{} accepted: {}", path, status);
            answers.entry(broker).or_default().insert((status, body));
            // Even with the trader's own session and token: no such route.
            let signed_in =
                with_session(post_json(&path, forged_fill(broker)), &cookie, Some(&csrf));
            let (status, v) = h.json(signed_in).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{}", path);
            assert_eq!(
                v,
                json!({"status": "error", "message": "Not found", "path": path})
            );
        }
    }
    for (broker, seen_answers) in &answers {
        assert_eq!(
            seen_answers.len(),
            1,
            "{}: the answer tells who is logged in",
            broker
        );
    }
    assert_eq!(delivered(&h, &seen).await, Vec::<String>::new());
}
