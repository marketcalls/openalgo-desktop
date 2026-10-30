//! The single-instrument order nodes against the web's executor tests
//! (`test_flow_default_product.py`, `test_flow_final_fix.py`,
//! `test_flow_qa_regressions.py`) and parity N-placeOrder..N-closePositions,
//! N-common-17/18/24.

use super::*;
use crate::flow::contract::client::app::NO_ANSWER;
use crate::flow::contract::client::UNKNOWN_OUTCOME;
use crate::flow::nodes::order_testing::{message, placed, Run, WORKFLOW};
use serde_json::json;

fn payload(run: &Run) -> Value {
    Value::Object(run.only_call().payload)
}

/// parity N-common-18: every sentence, Python float spelling included.
#[test]
fn prices_are_refused_with_the_web_sentences() {
    let cases = [
        (
            "LIMIT",
            f64::NAN,
            0.0,
            Some("LIMIT order needs a finite price, got nan"),
        ),
        (
            "sl",
            1.0,
            f64::INFINITY,
            Some("SL order needs a finite trigger price, got inf"),
        ),
        (
            "LIMIT",
            0.0,
            0.0,
            Some("LIMIT order needs a positive price, got 0.0"),
        ),
        (
            "SL",
            -2.5,
            1.0,
            Some("SL order needs a positive price, got -2.5"),
        ),
        (
            "SL-M",
            0.0,
            0.0,
            Some("SL-M order needs a positive trigger price, got 0.0"),
        ),
        ("", 0.0, 0.0, None),
        ("MARKET", 0.0, 0.0, None),
        ("SL-M", 0.0, 99.5, None),
    ];
    for (pt, price, trigger, want) in cases {
        assert_eq!(
            invalid_price_reason(pt, price, trigger).as_deref(),
            want,
            "{pt} {price} {trigger}"
        );
    }
}

/// parity N-common-17: the first bad field in the web's order is the one
/// reported, the symbol is upper-cased, the product follows the exchange.
#[tokio::test]
async fn the_first_bad_field_is_reported_and_nothing_is_sent() {
    let run = Run::new();
    let errors = run
        .walk(
            "placeOrder",
            json!({"symbol": "sbin", "exchange": "XX", "action": "HOLD", "quantity": 0}),
        )
        .await;
    assert!(errors[0]["message"]
        .as_str()
        .unwrap_or_default()
        .starts_with(
            "Place order has invalid runtime order data: exchange must be one of BCD, BFO,"
        ));
    assert!(run.calls().is_empty());
    let run = Run::new();
    let o = run.node("placeOrder", json!({"exchange": "NSE"})).await;
    assert_eq!(
        message(&o),
        "Place order has invalid runtime order data: symbol is required"
    );
    assert_eq!(run.log()[0].0, "error");
}

/// X-061..X-066: an order node follows its exchange for the product, and a
/// chosen product always wins.
#[tokio::test]
async fn the_product_follows_the_exchange_unless_chosen() {
    for (exchange, want) in [
        ("NSE", "MIS"),
        ("BSE", "MIS"),
        ("NFO", "NRML"),
        ("MCX", "NRML"),
        ("CDS", "NRML"),
    ] {
        let run = Run::new();
        run.reply("placeorder", placed());
        let data = json!({"symbol": "X", "exchange": exchange, "quantity": 1});
        run.node("placeOrder", data).await;
        assert_eq!(payload(&run)["product"], want, "{exchange}");
    }
    let run = Run::new();
    run.reply("placesmartorder", placed());
    let data = json!({"symbol": "NIFTY28AUG2624000CE", "exchange": "NFO", "quantity": 1, "positionSize": 1});
    run.node("smartOrder", data).await;
    assert_eq!(payload(&run)["product"], "NRML");
    let run = Run::new();
    run.reply("splitorder", placed());
    let data = json!({"symbol": "CRUDEOIL", "exchange": "MCX", "quantity": 2, "splitSize": 1});
    run.node("splitOrder", data).await;
    assert_eq!(payload(&run)["product"], "NRML");
    for (node, endpoint) in [
        ("placeOrder", "placeorder"),
        ("smartOrder", "placesmartorder"),
    ] {
        let run = Run::new();
        run.reply(endpoint, placed());
        let data = json!({"symbol": "X", "exchange": "NFO", "quantity": 1, "positionSize": 1, "product": "MIS"});
        run.node(node, data).await;
        assert_eq!(payload(&run)["product"], "MIS", "{node}");
    }
    let run = Run::new();
    run.reply("placeorder", placed());
    run.ctx.vars.set("webhook", json!({"exchange": "NFO"}));
    let data = json!({"symbol": "X", "exchange": "{{webhook.exchange}}", "quantity": 1});
    run.node("placeOrder", data).await;
    assert_eq!(payload(&run)["product"], "NRML");
    assert_eq!(payload(&run)["exchange"], "NFO");
}

/// X-081: a smart order keeps an explicit zero, with the client's payload
/// and the workflow's tag.
#[tokio::test]
async fn a_smart_order_keeps_a_resolved_zero_quantity() {
    let run = Run::new();
    run.reply("placesmartorder", placed());
    run.ctx.vars.set("runtime", json!(0));
    let data = json!({"action": "BUY", "product": "MIS", "priceType": "LIMIT", "price": 100,
        "symbol": "SBIN", "exchange": "NSE", "quantity": "{{runtime}}", "positionSize": 0});
    let o = run.node("smartOrder", data).await;
    assert!(!o.is_error());
    assert_eq!(
        payload(&run),
        json!({"strategy": WORKFLOW, "symbol": "SBIN", "exchange": "NSE", "action": "BUY",
               "quantity": 0, "position_size": 0, "pricetype": "LIMIT", "product": "MIS",
               "price": 100.0, "trigger_price": 0.0})
    );
    assert_eq!(
        run.messages(),
        [
            "Placing smart order: SBIN BUY",
            "Smart order result: {'orderid': '250408000123456', 'status': 'success'}"
        ]
    );
}

/// X-084 (executor half): trimmed, lower-case constants reach the client
/// canonical.
#[tokio::test]
async fn lower_case_constants_reach_the_client_canonical() {
    let run = Run::new();
    run.reply("placesmartorder", placed());
    let data = json!({"symbol": "SBIN", "exchange": " nse ", "action": " buy ", "quantity": 1,
        "positionSize": 0, "product": " mis ", "priceType": " limit ", "price": 100});
    run.node("smartOrder", data).await;
    let p = payload(&run);
    assert_eq!(
        (&p["exchange"], &p["action"], &p["product"], &p["pricetype"]),
        (&json!("NSE"), &json!("BUY"), &json!("MIS"), &json!("LIMIT"))
    );
}

/// X-183, X-184: smart and split prices reach the client; a template that
/// resolves to zero fails closed.
#[tokio::test]
async fn smart_and_split_prices_reach_the_client() {
    let run = Run::new();
    run.reply("placesmartorder", placed());
    let data = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 0,
        "positionSize": 5, "product": "MIS", "priceType": "LIMIT", "price": 625, "triggerPrice": 0});
    run.node("smartOrder", data).await;
    let p = payload(&run);
    assert_eq!(
        (&p["quantity"], &p["price"], &p["trigger_price"]),
        (&json!(0), &json!(625.0), &json!(0.0))
    );
    let run = Run::new();
    run.reply("splitorder", placed());
    let data = json!({"symbol": "SBIN", "exchange": "NSE", "action": "SELL", "quantity": 4,
        "splitSize": 2, "product": "MIS", "priceType": "SL", "price": 625, "triggerPrice": 624});
    run.node("splitOrder", data).await;
    let p = payload(&run);
    assert_eq!(
        (&p["quantity"], &p["price"], &p["trigger_price"]),
        (&json!(4), &json!(625.0), &json!(624.0))
    );
    assert_eq!(p["splitsize"], 2);
    let run = Run::new();
    run.ctx
        .vars
        .set("runtime", json!({"price": 0, "trigger": 0}));
    let smart = json!({"symbol": "SBIN", "exchange": "NSE", "quantity": 1, "positionSize": 1,
        "priceType": "LIMIT", "price": "{{runtime.price}}"});
    assert!(run.node("smartOrder", smart).await.is_error());
    let split = json!({"symbol": "SBIN", "exchange": "NSE", "quantity": 4, "splitSize": 2,
        "priceType": "SL", "price": 625, "triggerPrice": "{{runtime.trigger}}"});
    assert!(run.node("splitOrder", split).await.is_error());
    assert!(run.calls().is_empty());
}

/// The data an X-079/X-080/X-082/X-083 case sends for each node type.
fn order_data(node: &str, quantity: Value, price: Value) -> Value {
    let mut d = json!({"action": "BUY", "product": "MIS", "priceType": "LIMIT", "price": price});
    let extra = match node {
        "placeOrder" => json!({"symbol": "SBIN", "exchange": "NSE", "quantity": quantity}),
        "smartOrder" => {
            json!({"symbol": "SBIN", "exchange": "NSE", "quantity": quantity, "positionSize": 0})
        }
        "splitOrder" => {
            json!({"symbol": "SBIN", "exchange": "NSE", "quantity": quantity, "splitSize": 1})
        }
        "optionsOrder" => {
            json!({"underlying": "NIFTY", "expiryType": "current_week", "offset": "ATM", "optionType": "CE", "quantity": quantity})
        }
        "optionsMultiOrder" => {
            json!({"underlying": "NIFTY", "expiryType": "current_week", "strategy": "straddle", "quantity": quantity})
        }
        _ => {
            json!({"orders": [{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": quantity}]})
        }
    };
    if let (Some(m), Value::Object(e)) = (d.as_object_mut(), extra) {
        m.extend(e);
    }
    d
}

const SIX: [&str; 6] = [
    "placeOrder",
    "smartOrder",
    "optionsOrder",
    "optionsMultiOrder",
    "basketOrder",
    "splitOrder",
];

/// Run `node` on `data` with `{{runtime}}` = `runtime`; it must fail and
/// call nothing (not even the expiry lookup).
async fn refused(node: &str, runtime: Value, data: Value) {
    let run = Run::new();
    run.ctx.vars.set("runtime", runtime.clone());
    let o = run.node(node, data).await;
    assert!(o.is_error(), "{node} {runtime}: {:?}", o);
    assert!(
        run.calls().is_empty(),
        "{node} {runtime}: {:?}",
        run.calls()
    );
}

/// X-079, X-080: a resolved bad quantity never becomes an order size.
/// Python's NaN, infinity and 10**400 arrive as text here (JSON has no
/// such numbers), which takes the same coercion path.
#[tokio::test]
async fn a_resolved_bad_quantity_never_reaches_a_client() {
    let huge = format!("1{}", "0".repeat(400));
    let bad = [
        json!("not-a-number"),
        json!(-1),
        json!(true),
        json!("nan"),
        json!("inf"),
        json!(huge),
        json!(""),
    ];
    for node in SIX {
        for value in &bad {
            refused(
                node,
                value.clone(),
                order_data(node, json!("{{runtime}}"), json!(100)),
            )
            .await;
        }
        if node != "smartOrder" {
            refused(
                node,
                json!(0),
                order_data(node, json!("{{runtime}}"), json!(100)),
            )
            .await;
        }
    }
}

/// X-082, X-083: a LIMIT price or an SL trigger that resolves to zero or
/// a non-finite number never reaches a client.
#[tokio::test]
async fn a_resolved_bad_price_or_trigger_never_reaches_a_client() {
    for node in SIX {
        for value in [json!(0), json!("nan"), json!("inf"), json!("-inf")] {
            refused(
                node,
                value,
                order_data(node, json!(1), json!("{{runtime}}")),
            )
            .await;
        }
        for value in [json!(0), json!("nan"), json!("inf")] {
            let mut data = order_data(node, json!(1), json!(100));
            if node == "optionsMultiOrder" {
                data = json!({"underlying": "NIFTY", "expiryType": "current_week", "strategy": "custom",
                    "quantity": 1, "product": "MIS", "priceType": "SL", "price": 100,
                    "triggerPrice": "{{runtime}}",
                    "legs": [{"offset": "ATM", "optionType": "CE", "action": "BUY", "quantity": 1}]});
            } else if let Some(m) = data.as_object_mut() {
                m.insert("priceType".into(), json!("SL"));
                m.insert("triggerPrice".into(), json!("{{runtime}}"));
            }
            refused(node, value, data).await;
        }
    }
}

fn live_order() -> Value {
    json!({"status": "success", "data": {"orderid": "25082800018", "symbol": "YESBANK",
        "exchange": "NSE", "action": "SELL", "product": "NRML", "pricetype": "LIMIT",
        "quantity": "500", "price": 18.5, "trigger_price": 0}})
}

/// X-162, X-163, X-225: a modify keeps every attribute it does not change.
#[tokio::test]
async fn a_modify_keeps_the_live_orders_attributes() {
    let run = Run::new();
    run.reply("orderstatus", live_order()).reply(
        "modifyorder",
        json!({"status": "success", "orderid": "25082800018"}),
    );
    let o = run
        .node(
            "modifyOrder",
            json!({"orderId": "25082800018", "newPrice": "19.25"}),
        )
        .await;
    assert!(!o.is_error());
    let sent = Value::Object(run.client.calls_to("modifyorder")[0].payload.clone());
    assert_eq!(
        sent,
        json!({"strategy": "flow_workflow", "orderid": "25082800018", "symbol": "YESBANK",
               "exchange": "NSE", "action": "SELL", "quantity": 500, "pricetype": "LIMIT",
               "product": "NRML", "price": 19.25, "trigger_price": 0.0, "disclosed_quantity": 0})
    );
    assert_eq!(
        run.messages()[0],
        "Modifying order: 25082800018 - YESBANK SELL qty=500 price=19.25 trigger=0.0"
    );
    let run = Run::new();
    run.reply("orderstatus", live_order())
        .reply("modifyorder", placed());
    run.node(
        "modifyOrder",
        json!({"orderId": "25082800018", "newQuantity": "250"}),
    )
    .await;
    let sent = &run.client.calls_to("modifyorder")[0].payload;
    assert_eq!(
        (&sent["quantity"], &sent["price"]),
        (&json!(250), &json!(18.5))
    );
}

/// X-164 and parity N-modifyOrder-02..05: the refusals, each before a
/// modify is sent, in the web's words.
#[tokio::test]
async fn a_modify_refuses_what_it_cannot_read_or_parse() {
    let cases: [(Value, Value, &str); 6] = [
        (json!({}), live_order(), "orderId is required to modify an order"),
        (
            json!({"orderId": "7", "newPrice": 1}),
            json!({"status": "error", "error": "Invalid openalgo apikey", "code": 403}),
            "Could not read order 7 to modify it: Invalid openalgo apikey",
        ),
        (
            json!({"orderId": "7"}),
            json!({"status": "success", "data": {}}),
            "Could not read order 7 to modify it: order not found",
        ),
        (
            json!({"orderId": "7"}),
            json!({"status": "success", "data": {"symbol": "SBIN", "exchange": "NSE"}}),
            "Order 7 did not report action, product, priceType; refusing to modify it with a guessed value",
        ),
        (
            json!({"orderId": "7", "newPrice": "abc"}),
            live_order(),
            "Modify order got a non-numeric price or quantity: could not convert string to float: 'abc'",
        ),
        (
            json!({"orderId": "7", "newQuantity": "0"}),
            live_order(),
            "Modify order needs a positive quantity, got 0",
        ),
    ];
    for (data, status, want) in cases {
        let run = Run::new();
        run.reply("orderstatus", status);
        let o = run.node("modifyOrder", data).await;
        assert_eq!(message(&o), want);
        assert_eq!(
            run.log().last().map(|l| l.1.clone()),
            Some(format!("Modify order aborted: {want}"))
        );
        assert!(run.client.calls_to("modifyorder").is_empty());
    }
    // An infinite quantity is the web's uncaught OverflowError: the run ends.
    let run = Run::new();
    run.reply("orderstatus", live_order());
    let o = run
        .node("modifyOrder", json!({"orderId": "7", "newQuantity": "inf"}))
        .await;
    assert_eq!(
        o.aborted(),
        Some("cannot convert float infinity to integer")
    );
}

/// parity N-modifyOrder-07: a LIMIT modify to price 0 reaches the service
/// (it validates), as on the web.
#[tokio::test]
async fn a_modify_to_a_zero_price_reaches_the_service() {
    let run = Run::new();
    run.reply("orderstatus", live_order())
        .reply("modifyorder", placed());
    run.node("modifyOrder", json!({"orderId": "7", "newPrice": "0"}))
        .await;
    assert_eq!(run.client.calls_to("modifyorder")[0].payload["price"], 0.0);
}

/// WEB-DEFECT N-cancelOrder-02: the cancel result is stored like every
/// other action node's (the web never stored it, so `{{cancelled...}}`
/// stayed unresolved downstream). Fails on the web's behaviour.
#[tokio::test]
async fn a_cancel_stores_its_result() {
    let run = Run::new();
    let reply = json!({"status": "success", "orderid": "1"});
    run.reply("cancelorder", reply.clone());
    run.node(
        "cancelOrder",
        json!({"orderId": "1", "outputVariable": "cancelled"}),
    )
    .await;
    assert_eq!(run.ctx.vars.get("cancelled"), Some(reply));
    assert_eq!(run.messages()[2], "Stored result in variable: cancelled");
    // The id is sent as written, an empty one included (the service refuses).
    let run = Run::new();
    run.reply(
        "cancelorder",
        json!({"status": "error", "error": "Order ID is missing", "code": 400}),
    );
    assert!(run
        .node("cancelOrder", json!({"orderId": ""}))
        .await
        .is_error());
    assert_eq!(run.only_call().payload["orderid"], "");
    assert_eq!(run.messages()[0], "Cancelling order: ");
}

/// X-208: Cancel All Orders stores its result.
#[tokio::test]
async fn cancel_all_stores_its_result() {
    let run = Run::new();
    run.reply(
        "cancelallorder",
        json!({"status": "success", "cancelled": 3}),
    );
    run.node("cancelAllOrders", json!({"outputVariable": "cancelled"}))
        .await;
    assert_eq!(run.ctx.vars.interpolate("{{cancelled.cancelled}}"), "3");
}

/// X-165, X-067: only `symbol` scopes a close; the product follows the
/// exchange.
#[tokio::test]
async fn only_a_symbol_scopes_a_close() {
    for data in [
        json!({"symbol": "", "exchange": "NSE", "product": "MIS"}),
        json!({}),
    ] {
        let run = Run::new();
        run.reply("closeposition", json!({"status": "success"}));
        run.node("closePositions", data).await;
        assert_eq!(run.only_call().method, "close_all_positions");
        assert_eq!(run.messages()[0], "Closing all positions");
    }
    for (symbol, exchange, product) in [("SBIN", "NSE", "MIS"), ("GOLDM", "MCX", "NRML")] {
        let run = Run::new();
        run.reply("closeposition", json!({"status": "success"}));
        run.node(
            "closePositions",
            json!({"symbol": symbol, "exchange": exchange}),
        )
        .await;
        let call = run.only_call();
        assert_eq!(call.method, "close_position");
        assert_eq!(
            Value::Object(call.payload),
            json!({"strategy": WORKFLOW, "symbol": symbol, "exchange": exchange, "product": product})
        );
        assert_eq!(
            run.messages()[0],
            format!("Closing position: {symbol}@{exchange} ({product})")
        );
    }
}

/// decisions.md 11, CLAUDE.md "decides once": an order whose answer was
/// lost says it may have been placed, is sent once and stops the branch.
#[tokio::test]
async fn a_lost_answer_is_never_sent_again_and_stops_the_branch() {
    let run = Run::new();
    run.reply(
        "placeorder",
        json!({"status": "error", "error": NO_ANSWER, "code": 504, "error_type": UNKNOWN_OUTCOME}),
    );
    let errors = run
        .walk(
            "placeOrder",
            json!({"symbol": "SBIN", "exchange": "NSE", "quantity": 1}),
        )
        .await;
    assert_eq!(run.calls().len(), 1);
    assert_eq!(errors[0]["message"], NO_ANSWER);
    assert!(NO_ANSWER.contains("may have been placed"));
    assert_eq!(
        run.log().last().map(|l| l.1.clone()),
        Some(format!(
            "Stopping this branch: placeOrder node failed ({NO_ANSWER})"
        ))
    );
}

/// parity N-common-24: `_supplied` takes the first key whose interpolated
/// text is not blank.
#[test]
fn supplied_reads_the_first_filled_key() {
    let run = Run::new();
    run.ctx.vars.set("x", json!(" NFO "));
    let d = crate::flow::nodes::order_testing::object(
        json!({"a": "  ", "b": 0, "c": "{{x}}", "d": "NSE"}),
    );
    assert_eq!(
        supplied(&run.ctx, &d, &["a", "b", "c", "d"]).as_deref(),
        Some("NFO")
    );
    assert_eq!(supplied(&run.ctx, &d, &["a", "b", "missing"]), None);
}

/// parity N-placeOrder-00 .. N-splitOrder-00: the ten order node types
/// are registered, each once, and are the types the walk gates as orders
/// (cancelAllOrders aside, as on the web).
#[test]
fn the_ten_order_nodes_are_registered() {
    use crate::flow::contract::model::ORDER_TYPES;
    let registry = crate::flow::nodes::NodeRegistry::default();
    let types = [
        "placeOrder",
        "smartOrder",
        "optionsOrder",
        "optionsMultiOrder",
        "modifyOrder",
        "cancelOrder",
        "cancelAllOrders",
        "closePositions",
        "basketOrder",
        "splitOrder",
    ];
    for t in types {
        assert!(registry.get(t).is_some(), "{t} is not registered");
        assert_eq!(ORDER_TYPES.contains(&t), t != "cancelAllOrders", "{t}");
    }
}
