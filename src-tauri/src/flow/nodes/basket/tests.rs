//! Basket Order against the web's tests (`test_flow_default_product.py`
//! TestABasketDecidesPerRow, `test_flow_final_fix.py`,
//! `test_flow_qa_regressions.py`) and parity N-basketOrder-01..06.

use crate::flow::nodes::order_testing::{message, Run};
use serde_json::{json, Value};

fn ok() -> Value {
    json!({"status": "success", "results": []})
}

/// The rows sent in the one basket call.
async fn rows(run: &Run, data: Value) -> Vec<Value> {
    run.reply("basketorder", ok());
    let o = run.node("basketOrder", data).await;
    assert!(!o.is_error(), "{:?}", o);
    let call = run.only_call();
    call.payload["orders"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// WEB-DEFECT N-basketOrder-06: an SL row's trigger reaches the service
/// under `trigger_price`, the key the basket service reads (the web sent
/// `triggerprice`, so the trigger went out as 0). Fails on the web's rows.
#[tokio::test]
async fn an_sl_rows_trigger_reaches_the_order() {
    let run = Run::new();
    let sent = rows(
        &run,
        json!({"orders": "SBIN,NSE,SELL,10", "priceType": "SL", "price": 795, "triggerPrice": 796}),
    )
    .await;
    assert_eq!(sent[0]["trigger_price"], json!(796.0));
    assert!(sent[0].get("triggerprice").is_none());
    // The row the desktop's /api/v1/basketorder item schema loads, with no
    // unknown key (it refuses those).
    let body = json!({"apikey": "k", "strategy": "s", "orders": [sent[0]]});
    let loaded = crate::services::schemas::basket_order()
        .load(body.as_object().unwrap_or(&serde_json::Map::new()));
    assert!(loaded.is_ok(), "{:?}", loaded.err().map(|e| e.to_python()));
}

/// WEB-DEFECT N-basketOrder-04: a lower-case row symbol is upper-cased as
/// a single order's is.
#[tokio::test]
async fn a_lower_case_row_symbol_is_upper_cased() {
    let run = Run::new();
    let sent = rows(
        &run,
        json!({"orders": "sbin,nse,buy,10", "basketName": "morning"}),
    )
    .await;
    assert_eq!(sent[0]["symbol"], "SBIN");
    assert_eq!(run.messages()[1], "  Order 1: SBIN NSE BUY qty=10");
    assert_eq!(run.only_call().payload["strategy"], "morning");
}

/// X-071..X-073: each row follows its own exchange unless the node or the
/// row names a product.
#[tokio::test]
async fn each_row_decides_its_product() {
    let products = |rows: Vec<Value>| {
        rows.iter()
            .map(|r| r["product"].clone())
            .collect::<Vec<_>>()
    };
    let csv = "SBIN,NSE,BUY,1\nGOLDM,MCX,BUY,1";
    let run = Run::new();
    assert_eq!(
        products(rows(&run, json!({"orders": csv})).await),
        [json!("MIS"), json!("NRML")]
    );
    let run = Run::new();
    let both = rows(&run, json!({"orders": csv, "product": "NRML"})).await;
    assert_eq!(products(both), [json!("NRML"), json!("NRML")]);
    let run = Run::new();
    let listed = json!([
        {"symbol": "X", "exchange": "NFO", "action": "BUY", "quantity": 1},
        {"symbol": "Y", "exchange": "NFO", "action": "BUY", "quantity": 1, "product": "MIS"}
    ]);
    let own = rows(&run, json!({"orders": listed})).await;
    assert_eq!(products(own), [json!("NRML"), json!("MIS")]);
    assert_eq!(run.only_call().payload["strategy"], "flow_basket");
}

/// X-074 and the node-level checks reported as row 1 (parity
/// N-basketOrder-02, kept for parity).
#[tokio::test]
async fn a_bad_node_field_refuses_the_basket_as_row_1() {
    let cases = [
        (
            json!({"product": ""}),
            "Basket order row 1: product is required",
        ),
        (
            json!({"product": "BO"}),
            "Basket order row 1: invalid product 'BO'",
        ),
        (
            json!({"priceType": "STOPLOSS"}),
            "Basket order row 1: invalid pricetype 'STOPLOSS'",
        ),
        (
            json!({"price": true}),
            "Basket order row 1: price must be numeric, got boolean True",
        ),
        (
            json!({"price": " "}),
            "Basket order row 1: price must not be blank",
        ),
        (
            json!({"price": "cheap"}),
            "Basket order row 1: price must be numeric, got 'cheap'",
        ),
        (
            json!({"triggerPrice": "nan"}),
            "Basket order row 1: triggerPrice must be finite, got 'nan'",
        ),
    ];
    for (extra, want) in cases {
        let run = Run::new();
        let mut data = json!({"orders": "SBIN,NSE,BUY,1"});
        if let (Some(m), Value::Object(e)) = (data.as_object_mut(), extra) {
            m.extend(e);
        }
        let o = run.node("basketOrder", data).await;
        assert_eq!(message(&o), want);
        assert_eq!(run.messages(), [format!("Basket order failed: {want}")]);
        assert!(run.calls().is_empty());
    }
}

/// X-094: a list from a variable keeps its structure; each row's own value
/// wins over the node's; text values are trimmed and upper-cased.
#[tokio::test]
async fn a_list_template_resolves_row_by_row() {
    let run = Run::new();
    run.ctx.vars.set(
        "runtime",
        json!([
            {"symbol": " sbin ", "exchange": " nse ", "action": " buy ", "quantity": "2",
             "product": " cnc ", "pricetype": " limit ", "price": 100.5},
            {"symbol": "INFY", "exchange": "NSE", "action": "SELL", "quantity": 1}
        ]),
    );
    let sent = rows(
        &run,
        json!({"orders": "{{runtime}}", "product": " mis ", "priceType": " market "}),
    )
    .await;
    assert_eq!(
        sent,
        [
            json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 2, "product": "CNC",
                   "pricetype": "LIMIT", "price": 100.5, "trigger_price": 0.0}),
            json!({"symbol": "INFY", "exchange": "NSE", "action": "SELL", "quantity": 1, "product": "MIS",
                   "pricetype": "MARKET", "price": 0.0, "trigger_price": 0.0}),
        ]
    );
}

/// X-095, X-188..X-190: a bad container or row refuses the whole basket
/// before any call (never re-read as CSV, never partly sent).
#[tokio::test]
async fn a_bad_row_refuses_the_whole_basket() {
    let row = |extra: Value| {
        let mut r = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1});
        if let (Some(m), Value::Object(e)) = (r.as_object_mut(), extra) {
            m.extend(e);
        }
        r
    };
    let mut missing_symbol = row(json!({}));
    if let Some(m) = missing_symbol.as_object_mut() {
        m.remove("symbol");
    }
    let cases: Vec<(Value, &str)> = vec![
        (json!({}), "No valid orders to place"),
        (json!([]), "No valid orders to place"),
        (
            json!(["bad-row"]),
            "Basket order row 1: must be an order object",
        ),
        (
            json!([row(json!({"quantity": "{{missing.qty}}"}))]),
            "quantity has unresolved value",
        ),
        (json!([missing_symbol]), "symbol is required"),
        (
            json!([row(json!({"exchange": null}))]),
            "exchange is required",
        ),
        (json!([row(json!({"action": ""}))]), "action is required"),
        (json!("SBIN,BAD,BUY,1"), "invalid exchange 'BAD'"),
        (
            json!([row(json!({"action": "HOLD"}))]),
            "invalid action 'HOLD'",
        ),
        (
            json!([row(json!({"priceType": "BAD"}))]),
            "invalid pricetype 'BAD'",
        ),
        (
            json!([row(json!({})), row(json!({"exchange": "INVALID"}))]),
            "Basket order row 2: invalid exchange",
        ),
        (
            json!("SBIN,NSE,BUY,10\nINFY,NSE,SELL"),
            "Basket order row 2: expected SYMBOL,EXCHANGE,ACTION,QTY",
        ),
        (
            json!("SBIN,NSE,BUY,0"),
            "Basket order row 1: quantity must be positive, got 0",
        ),
    ];
    for (orders, want) in cases {
        let run = Run::new();
        run.ctx.vars.set("runtime", orders.clone());
        let o = run
            .node("basketOrder", json!({"orders": "{{runtime}}"}))
            .await;
        assert!(message(&o).contains(want), "{orders}: {}", message(&o));
        assert!(run.calls().is_empty(), "{orders}");
    }
    let run = Run::new();
    let o = run
        .node(
            "basketOrder",
            json!({"orders": "SBIN,NSE,BUY,1", "product": "BAD"}),
        )
        .await;
    assert!(message(&o).contains("product"));
}

/// X-185..X-187: node prices cover CSV rows; a list row keeps its own
/// prices and takes the node's where it has none; a bad price refuses;
/// the stored rows are not changed.
#[tokio::test]
async fn node_prices_cover_rows_and_bad_prices_refuse() {
    let run = Run::new();
    let node = json!({"orders": "SBIN,NSE,BUY,2", "product": "MIS", "priceType": "SL",
        "price": 625, "triggerPrice": 624});
    let sent = rows(&run, node).await;
    assert_eq!(
        sent[0],
        json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 2, "product": "MIS",
               "pricetype": "SL", "price": 625.0, "trigger_price": 624.0})
    );
    let run = Run::new();
    run.ctx
        .vars
        .set("runtime", json!({"price": 626, "trigger": 625}));
    let listed = json!([
        {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "product": "CNC",
         "priceType": "LIMIT", "price": "{{runtime.price}}", "triggerPrice": "{{runtime.trigger}}"},
        {"symbol": "INFY", "exchange": "NSE", "action": "BUY", "quantity": 1}
    ]);
    let data = json!({"orders": listed.clone(), "product": "MIS", "priceType": "SL",
        "price": 625, "triggerPrice": 624});
    let sent = rows(&run, data.clone()).await;
    assert_eq!(
        (
            &sent[0]["price"],
            &sent[0]["trigger_price"],
            &sent[0]["product"],
            &sent[0]["pricetype"]
        ),
        (&json!(626.0), &json!(625.0), &json!("CNC"), &json!("LIMIT"))
    );
    assert_eq!(
        (
            &sent[1]["price"],
            &sent[1]["trigger_price"],
            &sent[1]["product"],
            &sent[1]["pricetype"]
        ),
        (&json!(625.0), &json!(624.0), &json!("MIS"), &json!("SL"))
    );
    assert_eq!(data["orders"], listed, "the saved rows are not changed");
    let run = Run::new();
    run.ctx.vars.set("runtime", json!({"price": 0}));
    let csv =
        json!({"orders": "SBIN,NSE,BUY,1", "priceType": "LIMIT", "price": "{{runtime.price}}"});
    assert!(run.node("basketOrder", csv).await.is_error());
    let sl_row = json!({"orders": [{"symbol": "SBIN", "exchange": "NSE", "action": "BUY",
        "quantity": 1, "priceType": "SL", "price": 10, "triggerPrice": 0}]});
    assert!(run.node("basketOrder", sl_row).await.is_error());
    assert!(run.calls().is_empty());
}

/// N-basketOrder-03: a CSV row is numbered by its line, blank lines
/// counted (leading and trailing blank lines are trimmed first, as the
/// web's `strip()`).
#[tokio::test]
async fn a_csv_row_is_numbered_by_its_line() {
    for (orders, want) in [
        (
            "SBIN,NSE,BUY,1\n\nINFY,NSE",
            "Basket order row 3: expected SYMBOL,EXCHANGE,ACTION,QTY",
        ),
        (
            "SBIN,NSE,BUY,1\n   \n\nINFY,LSE,BUY,1",
            "Basket order row 4: invalid exchange 'LSE'",
        ),
        (
            "\n\nSBIN,NSE,BUY,1\nINFY,NSE,HOLD,1",
            "Basket order row 2: invalid action 'HOLD'",
        ),
    ] {
        let run = Run::new();
        let o = run.node("basketOrder", json!({"orders": orders})).await;
        assert_eq!(message(&o), want, "{orders:?}");
        assert!(run.calls().is_empty());
    }
}

/// N-basketOrder-04: a row with several faults reports the first in the
/// web's order: symbol, exchange text, action text, quantity, product and
/// price type text, then exchange, action, product, price type values,
/// then prices.
#[tokio::test]
async fn a_row_with_two_faults_reports_the_first_in_the_web_order() {
    let base = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1});
    let row = |changes: Value| {
        let mut r = base.clone();
        if let (Some(m), Value::Object(c)) = (r.as_object_mut(), changes) {
            for (k, v) in c {
                if v.is_null() {
                    m.remove(&k);
                } else {
                    m.insert(k, v);
                }
            }
        }
        r
    };
    let cases = [
        (
            row(json!({"symbol": null, "exchange": "BAD"})),
            "symbol is required",
        ),
        (
            row(json!({"exchange": "", "action": "HOLD"})),
            "exchange is required",
        ),
        (
            row(json!({"exchange": "BAD", "quantity": 0})),
            "quantity must be positive, got 0",
        ),
        (
            row(json!({"exchange": "BAD", "action": "HOLD"})),
            "invalid exchange 'BAD'",
        ),
        (
            row(json!({"action": "HOLD", "product": "BO"})),
            "invalid action 'HOLD'",
        ),
        (
            row(json!({"product": "BO", "pricetype": "IOC"})),
            "invalid product 'BO'",
        ),
        (
            row(json!({"pricetype": "IOC", "price": "x"})),
            "invalid pricetype 'IOC'",
        ),
        (
            row(json!({"price": "x", "triggerprice": "y"})),
            "price must be numeric, got 'x'",
        ),
        (
            row(json!({"pricetype": "LIMIT", "triggerprice": "y"})),
            "triggerprice must be numeric, got 'y'",
        ),
    ];
    for (r, want) in cases {
        let run = Run::new();
        let o = run
            .node("basketOrder", json!({"orders": [r.clone()]}))
            .await;
        assert_eq!(message(&o), format!("Basket order row 1: {want}"), "{r}");
    }
}
