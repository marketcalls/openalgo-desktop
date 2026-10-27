//! The account toolkit through the in-process `/api/v1`, live (mock broker)
//! and in analyzer mode (the sandbox), against the web's tests
//! (`test_agent_order_tool_arguments.py`, `test_gthread_integration_agent_reads.py`).

use super::*;
use crate::agent::tools::testkit::{body, tag, Kit};
use crate::brokers::types::{Order, Position};
use serde_json::json;

fn position(symbol: &str, quantity: i32) -> Position {
    Position {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity,
        overnight_quantity: 0,
        average_price: 100.0,
        ltp: 101.0,
        pnl: f64::from(quantity),
        realized_pnl: 0.0,
        unrealized_pnl: f64::from(quantity),
        buy_quantity: quantity.max(0),
        buy_value: 0.0,
        sell_quantity: 0,
        sell_value: 0.0,
    }
}

fn order(id: &str) -> Order {
    Order {
        order_id: id.into(),
        exchange_order_id: None,
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 1,
        filled_quantity: 1,
        pending_quantity: 0,
        price: 0.0,
        trigger_price: 0.0,
        average_price: 801.5,
        order_type: "MARKET".into(),
        product: "MIS".into(),
        status: "complete".into(),
        validity: "DAY".into(),
        order_timestamp: "09-Oct-2026 09:30:00".into(),
        exchange_timestamp: None,
        rejection_reason: None,
        order_tag: None,
    }
}

#[tokio::test]
async fn live_funds_carry_the_mode_and_currency() {
    let kit = Kit::new();
    let out = kit.call(&GROUP, "get_funds", json!({})).await;
    assert_eq!(tag(&out), "<tool_result tool=\"get_funds\">");
    let b = body(&out);
    assert_eq!(b["ok"], true);
    assert_eq!(b["mode"], "live");
    assert_eq!(b["currency"], "INR");
    assert_eq!(b["funds"]["availablecash"], "125000.50");
    assert!(b.get("mode_note").is_none());
}

#[tokio::test]
async fn analyzer_mode_figures_are_labelled_sandbox() {
    let kit = Kit::new();
    kit.analyzer(true);
    let out = kit.call(&GROUP, "get_funds", json!({})).await;
    let b = body(&out);
    assert_eq!(b["mode"], "analyze");
    assert_eq!(
        b["mode_note"],
        "Analyzer mode is on, so these are sandbox figures and not the live broker account. Say so when you report them."
    );
    // The sandbox, not the broker: no funds call reached the mock.
    assert_eq!(*kit.mock.funds_calls.lock(), 0);
    let positions = body(&kit.call(&GROUP, "get_positions", json!({})).await);
    assert_eq!(positions["mode"], "analyze");
    assert_eq!(positions["count"], 0);
}

#[tokio::test]
async fn an_empty_book_is_an_answer() {
    let kit = Kit::new();
    for (tool, key, note) in [
        (
            "get_positions",
            "positions",
            "There are no open positions. The position book is empty, so the operator is flat.",
        ),
        (
            "get_tradebook",
            "trades",
            "No trades were executed today. The trade book is empty.",
        ),
        (
            "get_orderbook",
            "orders",
            "No orders were placed today. The order book is empty.",
        ),
        (
            "get_holdings",
            "holdings",
            "There are no holdings. The delivery portfolio is empty.",
        ),
    ] {
        let out = kit.call(&GROUP, tool, json!({})).await;
        let b = body(&out);
        assert_eq!(b["count"], 0, "{}", tool);
        assert_eq!(b[key], json!([]), "{}", tool);
        assert_eq!(b["note"], note, "{}", tool);
    }
    let orders = body(&kit.call(&GROUP, "get_orderbook", json!({})).await);
    assert!(orders["statistics"].is_object(), "{}", orders);
}

#[tokio::test]
async fn a_long_book_keeps_whole_rows_and_says_how_many_were_dropped() {
    let kit = Kit::new();
    *kit.mock.positions.lock() = Some(Ok((0..300)
        .map(|i| position(&format!("SYM{}", i), 1))
        .collect()));
    let out = kit.call(&GROUP, "get_positions", json!({})).await;
    let b = body(&out);
    assert_eq!(b["count"], 300);
    let shown = b["positions"].as_array().map(Vec::len).unwrap_or_default();
    assert!(shown > 0 && shown < 300, "{}", shown);
    assert_eq!(b["rows_omitted"], 300 - shown);
    assert_eq!(
        b["note"],
        format!("Only the first {} of 300 rows are shown; {} were dropped so the result would fit. Report the count honestly, and ask about a specific symbol or order id if you need one of the rows that is missing.", shown, 300 - shown)
    );
    let inner = serde_json::to_string(&b).unwrap_or_default();
    assert!(inner.chars().count() <= MAX_RESULT_CHARS);
    assert!(b.get("truncated").is_none());
}

#[test]
fn a_book_in_an_unknown_shape_is_passed_through() {
    let (rows, extras, recognised) = split_book(&json!({"data": {"weird": 1}}), Some("orders"));
    assert!(rows.is_empty() && !recognised);
    assert_eq!(extras.get("weird"), Some(&json!(1)));
    let (rows, extras, recognised) = split_book(
        &json!({"data": {"orders": [1, 2], "statistics": {"a": 1}}}),
        Some("orders"),
    );
    assert_eq!((rows.len(), recognised), (2, true));
    assert!(extras.contains_key("statistics"));
    let (_, extras, recognised) = split_book(&json!({"data": "oops"}), None);
    assert!(!recognised && extras.get("data") == Some(&json!("oops")));
    assert!(split_book(&json!({"data": null}), None).2);
    let text = book_result("live", &POSITIONS, &[], &extras, false);
    assert!(text.contains(UNEXPECTED_SHAPE_NOTE), "{}", text);
}

#[tokio::test]
async fn the_open_position_note_does_not_claim_the_operator_is_flat() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "get_open_position",
            json!({"symbol": "sbin", "exchange": "nse", "product": "intraday"}),
        )
        .await;
    assert_eq!(
        tag(&out),
        "<tool_result tool=\"get_open_position\" symbol=\"SBIN\" exchange=\"NSE\">"
    );
    let b = body(&out);
    assert_eq!(b["product"], "MIS");
    assert_eq!(b["quantity"], 0);
    assert!(b.get("currency").is_none());
    let note = b["note"].as_str().unwrap_or_default();
    assert!(!note.contains("flat in this contract"));
    assert!(note.contains("reported no open quantity") && note.contains("get_positions"));

    *kit.mock.positions.lock() = Some(Ok(vec![position("SBIN", 50)]));
    let b = body(
        &kit.call(
            &GROUP,
            "get_open_position",
            json!({"symbol": "SBIN", "exchange": "NSE", "product": "MIS"}),
        )
        .await,
    );
    assert_eq!(b["quantity"], 50);
    assert!(b.get("note").is_none());
}

#[tokio::test]
async fn open_position_refusals_name_the_argument() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "get_open_position",
            json!({"symbol": "SBIN", "exchange": "LSE", "product": "MIS"}),
        )
        .await;
    assert_eq!(
        out,
        ToolOutcome::Retry(format!(
            "The 'exchange' argument is invalid: 'LSE' is not an OpenAlgo exchange code. Use one of: {}.",
            VALID_EXCHANGES.join(", ")
        ))
    );
    let out = kit
        .call(
            &GROUP,
            "get_open_position",
            json!({"symbol": "SBIN", "exchange": "NSE", "product": "BO"}),
        )
        .await;
    assert_eq!(
        out,
        ToolOutcome::Retry("The 'product' argument is invalid: 'BO' is not an OpenAlgo product type. Use one of: CNC, NRML, MIS. CNC is delivery equity, NRML is carry futures and options, MIS is intraday.".into())
    );
    let out = kit
        .call(
            &GROUP,
            "get_open_position",
            json!({"symbol": "  ", "exchange": "NSE", "product": "MIS"}),
        )
        .await;
    assert_eq!(
        out,
        ToolOutcome::Retry(
            "The 'symbol' argument is invalid: it is empty. Supply a real value.".into()
        )
    );
}

#[tokio::test]
async fn a_numeric_order_id_is_looked_up_as_its_text() {
    let kit = Kit::new();
    *kit.mock.order_book.lock() = Some(Ok(vec![order("250408000989443")]));
    let out = kit
        .call(
            &GROUP,
            "get_order_status",
            json!({"order_id": 250408000989443_u64}),
        )
        .await;
    assert_eq!(
        tag(&out),
        "<tool_result tool=\"get_order_status\" order_id=\"250408000989443\">"
    );
    let b = body(&out);
    assert_eq!(b["found"], true);
    assert_eq!(b["order_id"], "250408000989443");
    assert_eq!(b["order"]["orderid"], "250408000989443");
}

#[tokio::test]
async fn an_unknown_order_id_is_an_answer_naming_it() {
    let kit = Kit::new();
    let out = kit
        .call(&GROUP, "get_order_status", json!({"order_id": "123"}))
        .await;
    assert!(out.is_ok(), "{:?}", out);
    let b = body(&out);
    assert_eq!(b["found"], false);
    assert_eq!(
        b["note"],
        "No order with id 123 is in today's order book. That is an answer, not a failure: the id may belong to an earlier session, or the order was never accepted. Call get_orderbook to see today's orders."
    );
}

#[test]
fn flat_means_a_quantity_that_parses_as_zero() {
    assert!(looks_flat(&json!(0)));
    assert!(looks_flat(&json!("0.0")));
    assert!(looks_flat(&json!(" 0 ")));
    assert!(!looks_flat(&json!(5)));
    assert!(!looks_flat(&json!("n/a")));
    assert!(!looks_flat(&json!(null)));
    assert_eq!(canonical_product("intraday"), "MIS");
    assert_eq!(canonical_product("Delivery"), "CNC");
    assert_eq!(canonical_product("carry_forward"), "NRML");
    assert_eq!(canonical_product("bo"), "BO");
}

#[tokio::test]
async fn the_account_toolkit_matches_the_web() {
    let kit = Kit::new();
    let ctx = kit.ctx(crate::agent::contract::tool::Surface::Voice);
    crate::agent::tools::spec_contract::assert_toolkit(&GROUP, &ctx);
}
