//! Strategy P&L: `pnl_from_book` (web `test/test_strategy_pnl_leg_order.py`,
//! ported) and the node over a strategy book and a position book.

use super::support::{ctx_at, env_with, logs, run, FakeBook, FakeCalendar};
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::nodes::books::pnl_from_book;
use serde_json::{json, Value};
use std::sync::Arc;

fn leg(symbol: &str, quantity: f64, average_price: f64, realized: f64) -> Value {
    json!({
        "strategy": "S", "symbol": symbol, "exchange": "NFO", "product": "NRML",
        "quantity": quantity, "average_price": average_price,
        "realized_pnl": realized, "today_realized_pnl": 0.0,
    })
}

fn symbols(r: &serde_json::Map<String, Value>) -> Vec<String> {
    r["legs"]
        .as_array()
        .map(|l| {
            l.iter()
                .map(|x| x["symbol"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn an_open_leg_sorts_ahead_of_an_older_flat_leg() {
    let legs = [
        leg("NIFTY04AUG2624300PE", 0.0, 0.0, 120.0),
        leg("NIFTY11AUG2624450PE", 65.0, 45.75, 0.0),
    ];
    let positions = [
        json!({"symbol": "NIFTY11AUG2624450PE", "exchange": "NFO", "product": "NRML", "ltp": 50.5}),
    ];
    let r = pnl_from_book(&legs, &positions, "S");
    assert_eq!(symbols(&r)[0], "NIFTY11AUG2624450PE");
    assert_eq!(r["legs"][0]["average_price"], 45.75);
    assert_eq!(r["open_quantity"], 65.0);
    assert_eq!(r["unrealized"], 308.75);
    assert_eq!(r["total"], 428.75);
}

#[test]
fn a_short_leg_counts_as_open_and_open_legs_keep_their_order() {
    let r = pnl_from_book(
        &[leg("FLAT", 0.0, 0.0, 0.0), leg("SHORT", -25.0, 8.0, 0.0)],
        &[],
        "S",
    );
    assert_eq!(symbols(&r)[0], "SHORT");
    assert_eq!(r["open_quantity"], -25.0);
    assert_eq!(
        r["unpriced_legs"], 1,
        "an open leg with no price is surfaced"
    );
    let r = pnl_from_book(
        &[
            leg("A", 0.0, 0.0, 0.0),
            leg("B", 50.0, 10.0, 0.0),
            leg("C", -25.0, 8.0, 0.0),
        ],
        &[],
        "S",
    );
    assert_eq!(symbols(&r), vec!["B", "C", "A"]);
}

#[test]
fn a_flat_strategy_keeps_its_closed_legs_and_an_unknown_one_is_flat() {
    let mut closed = leg("CLOSED", 0.0, 0.0, 120.0);
    closed["today_realized_pnl"] = json!(120.0);
    let r = pnl_from_book(&[closed], &[], "S");
    assert_eq!(r["open_quantity"], 0.0);
    assert_eq!(symbols(&r), vec!["CLOSED"]);
    assert_eq!(r["realized"], 120.0);
    assert_eq!(r["today_total"], 120.0);
    let r = pnl_from_book(&[], &[], "never-traded");
    assert_eq!(
        Value::Object(r),
        json!({"strategy": "never-traded", "realized": 0.0, "today_realized": 0.0,
               "unrealized": 0.0, "total": 0.0, "today_total": 0.0, "open_quantity": 0.0,
               "unpriced_legs": 0, "legs": []})
    );
}

#[test]
fn a_price_falls_back_to_last_price_and_totals_round_to_four_places() {
    let legs = [leg("X", 3.0, 100.123456, 0.333333)];
    let positions =
        [json!({"symbol": "X", "exchange": "NFO", "product": "NRML", "last_price": 101.0})];
    let r = pnl_from_book(&legs, &positions, "S");
    assert_eq!(r["legs"][0]["ltp"], 101.0);
    assert_eq!(r["legs"][0]["average_price"], 100.1235);
    assert_eq!(r["unrealized"], 2.6296);
    assert_eq!(r["realized"], 0.3333);
}

/// The node: the workflow's name by default, the web's log lines, and an
/// unreadable book or position book reported, never a zero.
#[tokio::test]
async fn the_node_reports_and_refuses_as_the_web() {
    let book = vec![
        json!({"strategy": "Gap fade", "symbol": "SBIN", "exchange": "NSE", "product": "MIS",
               "quantity": 10.0, "average_price": 800.0, "realized_pnl": 50.0, "today_realized_pnl": 50.0}),
    ];
    let client = Arc::new(FakeFlowClient::new());
    client.reply(
        "positionbook",
        json!({"status": "success", "data": [{"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "ltp": 812.5}]}),
    );
    let env = env_with(FakeBook(Ok(book)), FakeCalendar::default());
    let ctx = ctx_at(client, "2026-10-29T10:15:00", "Gap fade", env);
    let out = run(&ctx, "strategyPnl", json!({"outputVariable": "pnl"})).await;
    let v = out.value().cloned().unwrap_or(Value::Null);
    assert_eq!(v["strategy"], "Gap fade");
    assert_eq!(v["total"], 175.0);
    assert_eq!(
        logs(&ctx)[0].1,
        "Strategy P&L [Gap fade]: realized=50.0 unrealized=125.0 total=175.0 openQty=10.0"
    );
    assert_eq!(ctx.vars.get("pnl"), Some(v));

    let down = Arc::new(FakeFlowClient::new());
    down.reply_error("positionbook", "Broker session expired", 401);
    let env = env_with(FakeBook(Ok(Vec::new())), FakeCalendar::default());
    let ctx = ctx_at(down, "2026-10-29T10:15:00", "Gap fade", env);
    let out = run(&ctx, "strategyPnl", json!({"outputVariable": "pnl"})).await;
    assert_eq!(
        out.value().cloned(),
        Some(
            json!({"status": "error", "message": "Position book unavailable, cannot value open legs: Broker session expired"})
        )
    );
    assert_eq!(ctx.vars.get("pnl"), None, "an error is not stored");
}
