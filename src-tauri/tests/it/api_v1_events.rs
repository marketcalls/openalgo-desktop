//! Every order endpoint publishes the web's event on the bus (live and
//! analyzer mode), with the web's payload fields and never the API key;
//! analyzer toggling starts and stops the sandbox engine.

use crate::api_v1_support;

use api_v1_support::H;
use openalgo_desktop_lib::brokers::types::{Order, Position};
use openalgo_desktop_lib::events::{Event, Mode};
use serde_json::{json, Value};

fn order_body() -> Value {
    json!({"strategy": "t", "exchange": "NSE", "symbol": "SBIN", "action": "BUY",
        "quantity": 1, "pricetype": "MARKET", "product": "MIS"})
}

async fn call(h: &H, path: &str, body: Value) -> (u16, Value) {
    let (s, v) = h.post(path, h.with_key(body)).await;
    (s.as_u16(), v)
}

/// Run one call and return the topics it published.
async fn topics_of(
    h: &H,
    path: &str,
    body: Value,
    expect: usize,
) -> (u16, Value, Vec<&'static str>) {
    h.recorder.clear();
    let (s, v) = call(h, path, body).await;
    let ev = h.recorder.wait_for(expect).await;
    (s, v, ev.iter().map(|e| e.topic().as_str()).collect())
}

fn no_key_anywhere(h: &H) {
    for e in h.recorder.events.lock().iter() {
        if let Some(m) = e.meta() {
            let text = m.request_data.to_string();
            assert!(
                !text.contains(&h.key),
                "API key leaked into {:?}",
                e.topic()
            );
            assert!(m.request_data.get("apikey").is_none());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_order_endpoints_publish_web_events() {
    let h = H::new().await;
    h.analyze(false);

    let (s, v, t) = topics_of(&h, "/api/v1/placeorder", order_body(), 1).await;
    assert_eq!(
        (s, t.as_slice()),
        (200, ["order.placed"].as_slice()),
        "{}",
        v
    );
    assert_eq!(v, json!({"status": "success", "orderid": "MOCK-1"}));
    {
        let ev = h.recorder.events.lock();
        match ev[0].as_ref() {
            Event::OrderPlaced {
                meta,
                symbol,
                exchange,
                orderid,
                action,
                quantity,
                ..
            } => {
                assert_eq!(meta.mode, Mode::Live);
                assert_eq!(meta.api_type, "placeorder");
                assert_eq!(
                    (symbol.as_str(), exchange.as_str(), orderid.as_str()),
                    ("SBIN", "NSE", "MOCK-1")
                );
                assert_eq!((action.as_str(), *quantity), ("BUY", 1));
            }
            other => panic!("unexpected {:?}", other.topic()),
        }
    }

    h.mock
        .order_ids
        .lock()
        .push_back(Err("Insufficient margin".into()));
    let (s, v, t) = topics_of(&h, "/api/v1/placeorder", order_body(), 1).await;
    assert_eq!((s, t.as_slice()), (400, ["order.failed"].as_slice()));
    assert_eq!(v["message"], "Insufficient margin");

    // Smart order: flat -> 5 places; already at 5 is a no-action.
    let mut smart = order_body();
    smart["position_size"] = json!(5);
    smart["quantity"] = json!(5);
    let (s, _, t) = topics_of(&h, "/api/v1/placesmartorder", smart.clone(), 1).await;
    assert_eq!((s, t.as_slice()), (200, ["order.placed"].as_slice()));
    *h.mock.positions.lock() = Some(Ok(vec![Position {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity: 5,
        overnight_quantity: 0,
        average_price: 800.0,
        ltp: 801.0,
        pnl: 5.0,
        realized_pnl: 0.0,
        unrealized_pnl: 5.0,
        buy_quantity: 5,
        buy_value: 4000.0,
        sell_quantity: 0,
        sell_value: 0.0,
    }]));
    let (s, v, t) = topics_of(&h, "/api/v1/placesmartorder", smart, 1).await;
    assert_eq!((s, t.as_slice()), (200, ["order.no_action"].as_slice()));
    assert_eq!(
        v,
        json!({"status": "success", "message": "Positions Already Matched. No Action needed."})
    );

    let modify = json!({"strategy": "t", "exchange": "NSE", "symbol": "SBIN", "orderid": "MOCK-1",
        "action": "BUY", "product": "MIS", "pricetype": "LIMIT", "price": 800, "quantity": 1,
        "disclosed_quantity": 0, "trigger_price": 0});
    let (s, v, t) = topics_of(&h, "/api/v1/modifyorder", modify.clone(), 1).await;
    assert_eq!((s, t.as_slice()), (200, ["order.modified"].as_slice()));
    assert_eq!(v, json!({"status": "success", "orderid": "MOCK-1"}));
    *h.mock.modify.lock() = Some(Err("Order already complete".into()));
    let (s, _, t) = topics_of(&h, "/api/v1/modifyorder", modify, 1).await;
    assert_eq!((s, t.as_slice()), (400, ["order.modify_failed"].as_slice()));

    let (s, _, t) = topics_of(
        &h,
        "/api/v1/cancelorder",
        json!({"strategy": "t", "orderid": "MOCK-1"}),
        1,
    )
    .await;
    assert_eq!((s, t.as_slice()), (200, ["order.cancelled"].as_slice()));
    *h.mock.cancel.lock() = Some(Err("Order not found".into()));
    let (s, _, t) = topics_of(
        &h,
        "/api/v1/cancelorder",
        json!({"strategy": "t", "orderid": "X"}),
        1,
    )
    .await;
    assert_eq!((s, t.as_slice()), (400, ["order.cancel_failed"].as_slice()));
    *h.mock.cancel.lock() = None;

    *h.mock.order_book.lock() = Some(Ok(vec![Order {
        order_tag: None,
        order_id: "MOCK-9".into(),
        exchange_order_id: None,
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 1,
        filled_quantity: 0,
        pending_quantity: 1,
        price: 700.0,
        trigger_price: 0.0,
        average_price: 0.0,
        order_type: "LIMIT".into(),
        product: "MIS".into(),
        status: "open".into(),
        validity: "DAY".into(),
        order_timestamp: "2026-10-05 10:00:00".into(),
        exchange_timestamp: None,
        rejection_reason: None,
    }]));
    let (s, v, t) = topics_of(&h, "/api/v1/cancelallorder", json!({"strategy": "t"}), 1).await;
    assert_eq!(
        (s, t.as_slice()),
        (200, ["orders.all_cancelled"].as_slice())
    );
    assert_eq!(v["canceled_orders"], json!(["MOCK-9"]));
    assert_eq!(
        v["message"],
        "Canceled 1 orders. Failed to cancel 0 orders."
    );

    let (s, v, t) = topics_of(&h, "/api/v1/closeposition", json!({"strategy": "t"}), 1).await;
    assert_eq!((s, t.as_slice()), (200, ["position.closed"].as_slice()));
    assert_eq!(
        v,
        json!({"status": "success", "message": "All Open Positions Squared Off"})
    );

    let basket = json!({"strategy": "t", "orders": [
        {"exchange": "NSE", "symbol": "SBIN", "action": "SELL", "quantity": 1},
        {"exchange": "NSE", "symbol": "INFY", "action": "BUY", "quantity": 1}]});
    let (s, v, t) = topics_of(&h, "/api/v1/basketorder", basket, 1).await;
    assert_eq!((s, t.as_slice()), (200, ["basket.completed"].as_slice()));
    // BUY legs first.
    assert_eq!(v["results"][0]["symbol"], "INFY");

    let mut split = order_body();
    split["quantity"] = json!(5);
    split["splitsize"] = json!(2);
    let (s, v, t) = topics_of(&h, "/api/v1/splitorder", split, 1).await;
    assert_eq!((s, t.as_slice()), (200, ["split.completed"].as_slice()));
    let q: Vec<i64> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["quantity"].as_i64().unwrap())
        .collect();
    assert_eq!(q, [2, 2, 1]);

    let opt = json!({"strategy": "t", "underlying": "NIFTY", "exchange": "NSE_INDEX",
        "expiry_date": "06OCT26", "offset": "ATM", "option_type": "CE", "action": "BUY",
        "quantity": 65, "product": "NRML"});
    let (s, v, t) = topics_of(&h, "/api/v1/optionsorder", opt.clone(), 1).await;
    assert_eq!(
        (s, t.as_slice()),
        (200, ["order.placed"].as_slice()),
        "{}",
        v
    );
    assert!(v.get("mode").is_none(), "live has no mode key: {}", v);
    let mut opt_split = opt.clone();
    opt_split["quantity"] = json!(130);
    opt_split["splitsize"] = json!(65);
    let (s, _, t) = topics_of(&h, "/api/v1/optionsorder", opt_split, 1).await;
    assert_eq!((s, t.as_slice()), (200, ["options.completed"].as_slice()));

    let multi = json!({"strategy": "t", "underlying": "NIFTY", "exchange": "NSE_INDEX",
        "expiry_date": "06OCT26", "legs": [
            {"offset": "ATM", "option_type": "CE", "action": "BUY", "quantity": 65},
            {"offset": "OTM2", "option_type": "CE", "action": "SELL", "quantity": 65}]});
    let (s, v, t) = topics_of(&h, "/api/v1/optionsmultiorder", multi, 1).await;
    assert_eq!(
        (s, t.as_slice()),
        (200, ["multiorder.completed"].as_slice()),
        "{}",
        v
    );

    let gtt = json!({"strategy": "t", "trigger_type": "SINGLE", "exchange": "NSE", "symbol": "SBIN",
        "action": "BUY", "product": "CNC", "quantity": 1, "price": 900, "triggerprice_sl": 905});
    let (s, _, t) = topics_of(&h, "/api/v1/placegttorder", gtt.clone(), 1).await;
    assert_eq!((s, t.as_slice()), (200, ["gtt.placed"].as_slice()));
    let mut m = gtt.clone();
    m["trigger_id"] = json!("GTT-1");
    let (s, _, t) = topics_of(&h, "/api/v1/modifygttorder", m, 1).await;
    assert_eq!((s, t.as_slice()), (200, ["gtt.modified"].as_slice()));
    let (s, _, t) = topics_of(
        &h,
        "/api/v1/cancelgttorder",
        json!({"strategy": "t", "trigger_id": "GTT-1"}),
        1,
    )
    .await;
    assert_eq!((s, t.as_slice()), (200, ["gtt.cancelled"].as_slice()));

    // A live validation failure on an order endpoint is published too.
    let (s, v, t) = topics_of(&h, "/api/v1/cancelorder", json!({"strategy": "t"}), 1).await;
    assert_eq!((s, t.as_slice()), (400, ["order.failed"].as_slice()));
    assert_eq!(
        v["message"],
        "{'orderid': ['Missing data for required field.']}"
    );
    assert!(v.get("mode").is_none());

    no_key_anywhere(&h);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analyzer_mode_never_reaches_the_broker_and_publishes_analyze_events() {
    let h = H::new().await;
    h.analyze(true);
    let before = h
        .mock
        .calls()
        .iter()
        .filter(|c| format!("{:?}", c).starts_with("PlaceOrder"))
        .count();

    let (s, v, t) = topics_of(&h, "/api/v1/placeorder", order_body(), 1).await;
    assert_eq!(
        (s, t.as_slice()),
        (200, ["order.placed"].as_slice()),
        "{}",
        v
    );
    assert_eq!(v["mode"], "analyze");
    {
        let ev = h.recorder.events.lock();
        assert_eq!(ev[0].meta().unwrap().mode, Mode::Analyze);
    }
    let id = v["orderid"].as_str().unwrap().to_string();

    let (s, _, t) = topics_of(
        &h,
        "/api/v1/cancelorder",
        json!({"strategy": "t", "orderid": id}),
        1,
    )
    .await;
    assert_eq!((s, t.as_slice()), (400, ["order.cancel_failed"].as_slice()));

    let mut smart = order_body();
    smart["position_size"] = json!(1);
    smart["quantity"] = json!(1);
    let (s, v, t) = topics_of(&h, "/api/v1/placesmartorder", smart, 1).await;
    assert_eq!(
        (s, t.as_slice()),
        (200, ["order.no_action"].as_slice()),
        "{}",
        v
    );

    // Schema failure in analyzer mode: analyze body and analyzer.error.
    let (s, v, t) = topics_of(&h, "/api/v1/modifyorder", json!({"strategy": "t"}), 1).await;
    assert_eq!((s, t.as_slice()), (400, ["analyzer.error"].as_slice()));
    assert_eq!(v["mode"], "analyze");
    assert!(v["message"]
        .as_str()
        .unwrap()
        .starts_with("{'exchange': ['Missing data for required field.']"));

    let (s, _, t) = topics_of(&h, "/api/v1/cancelallorder", json!({"strategy": "t"}), 1).await;
    assert_eq!(
        (s, t.as_slice()),
        (200, ["orders.all_cancelled"].as_slice())
    );
    let (s, v, t) = topics_of(&h, "/api/v1/closeposition", json!({"strategy": "t"}), 1).await;
    assert_eq!((s, t.as_slice()), (200, ["position.closed"].as_slice()));
    assert_eq!(v["mode"], "analyze");

    let after = h
        .mock
        .calls()
        .iter()
        .filter(|c| format!("{:?}", c).starts_with("PlaceOrder"))
        .count();
    assert_eq!(before, after, "an analyzer-mode order reached the broker");
    no_key_anywhere(&h);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analyzer_toggle_starts_and_stops_the_sandbox_engine() {
    let h = H::new().await;
    h.analyze(false);
    assert!(!h.ctx.sandbox.is_engine_running().await);
    let (s, v) = call(&h, "/api/v1/analyzer/toggle", json!({"mode": true})).await;
    assert_eq!(s, 200);
    assert_eq!(v["data"]["message"], "Analyzer mode switched to analyze");
    assert!(h.ctx.sandbox.is_engine_running().await);
    let (s, v) = call(&h, "/api/v1/analyzer/toggle", json!({"mode": "false"})).await;
    assert_eq!(s, 200);
    assert_eq!(v["data"]["mode"], "live");
    assert!(!h.ctx.sandbox.is_engine_running().await);
    let (s, v) = call(&h, "/api/v1/analyzer/toggle", json!({"mode": "maybe"})).await;
    assert_eq!(
        (s, v["message"].as_str().unwrap()),
        (400, "{'mode': ['Not a valid boolean.']}")
    );
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_live_skips_the_analyzer_toggle() {
    use openalgo_desktop_lib::services::{order_service, Route};
    let h = H::new().await;
    h.analyze(true);
    let req = h.with_key(order_body());
    let r = order_service::place_order(&h.ctx, &req, Route::LIVE).await;
    assert_eq!(r.body, json!({"status": "success", "orderid": "MOCK-1"}));
    let r = order_service::place_order(&h.ctx, &req, Route::API).await;
    assert_eq!(r.body["mode"], "analyze");
    h.shutdown().await;
}

#[tokio::test]
async fn holdings_statistics_use_the_brokers_own_totals() {
    use openalgo_desktop_lib::brokers::types::{Holding, PortfolioStats};
    let h = H::new().await;
    let row = Holding {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        product: "CNC".into(),
        isin: None,
        quantity: 10,
        t1_quantity: 0,
        average_price: 800.0,
        ltp: 810.0,
        close_price: 805.0,
        pnl: 100.0,
        pnl_percentage: 1.25,
        current_value: 8100.0,
    };
    *h.mock.holdings.lock() = Some(Ok(vec![row]));
    // Without broker totals the statistics are computed from the rows.
    let (_, v) = h.post("/api/v1/holdings", h.with_key(json!({}))).await;
    assert_eq!(v["data"]["statistics"]["totalholdingvalue"], json!(8100.0));
    assert_eq!(v["data"]["statistics"]["totalinvvalue"], json!(8000.0));
    // A broker that reports its own totals (Angel) is shown as reported.
    *h.mock.holdings_totals.lock() = Some(PortfolioStats {
        totalholdingvalue: 8123.45,
        totalinvvalue: 8000.0,
        totalprofitandloss: 123.45,
        totalpnlpercentage: 1.54,
    });
    let (_, v) = h.post("/api/v1/holdings", h.with_key(json!({}))).await;
    assert_eq!(v["status"], "success");
    assert_eq!(v["data"]["statistics"]["totalholdingvalue"], json!(8123.45));
    assert_eq!(v["data"]["statistics"]["totalprofitandloss"], json!(123.45));
    assert_eq!(v["data"]["statistics"]["totalpnlpercentage"], json!(1.54));
    assert_eq!(v["data"]["holdings"][0]["symbol"], "SBIN");
    h.shutdown().await;
}
