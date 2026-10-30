//! The Margin node's choices (web `execute_margin` and
//! `_parse_margin_positions`): which basket key it reads, when the basket
//! wins over the single position, the single position's exchange default,
//! and the order nodes' price check it shares (decision 36, N-margin-03).

use super::support::{ctx, logs, run};
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::nodes::orders::invalid_price_reason;
use serde_json::{json, Value};
use std::sync::Arc;

fn ok_margin() -> Value {
    json!({"status": "success", "data": {"total_margin_required": 1.0}})
}

/// One basket position as the margin service receives it.
fn sent(symbol: &str) -> Value {
    json!({"symbol": symbol, "exchange": "NSE", "action": "BUY", "quantity": "1",
           "product": "MIS", "pricetype": "MARKET"})
}

fn basket(symbol: &str) -> String {
    json!([{"symbol": symbol, "exchange": "NSE", "action": "BUY", "quantity": 1,
            "product": "MIS", "pricetype": "MARKET"}])
    .to_string()
}

/// Run a margin node and answer what the service received.
async fn margin_sent(data: Value) -> (Option<Value>, Vec<(String, String)>) {
    let client = Arc::new(FakeFlowClient::new());
    client.reply("margin", ok_margin());
    let c = ctx(client.clone());
    run(&c, "margin", data).await;
    let positions = client
        .calls_to("margin")
        .first()
        .map(|call| call.payload["positions"].clone());
    (positions, logs(&c))
}

/// N-margin-01: a single position with no exchange is priced on NSE (and
/// so as MIS, NSE's default product).
#[tokio::test]
async fn a_single_position_defaults_to_nse() {
    let (positions, logs) = margin_sent(json!({"symbol": "SBIN", "quantity": 2})).await;
    assert_eq!(
        positions,
        Some(
            json!([{"exchange": "NSE", "symbol": "SBIN", "action": "BUY", "quantity": "2",
                    "product": "MIS", "pricetype": "MARKET", "price": "0"}])
        )
    );
    assert!(logs
        .iter()
        .any(|(_, m)| m == "Calculating margin for: SBIN (NSE)"));
}

/// N-margin-02: `positionsJson` is read before `positions`; a null one is
/// absent, so `positions` is read.
#[tokio::test]
async fn positions_json_is_read_before_positions() {
    let (both, _) =
        margin_sent(json!({"positionsJson": basket("SBIN"), "positions": basket("INFY")})).await;
    assert_eq!(both, Some(json!([sent("SBIN")])));
    let (null_first, _) =
        margin_sent(json!({"positionsJson": null, "positions": basket("INFY")})).await;
    assert_eq!(null_first, Some(json!([sent("INFY")])));
}

/// N-margin-06: a blank `positionsJson` (the editor's emptied basket) is
/// absent, so a `positions` basket behind it is read (the web stopped at the
/// blank key and failed the run).
#[tokio::test]
async fn a_blank_positions_json_falls_back_to_positions() {
    for blank in ["", "   "] {
        let (positions, _) = margin_sent(
            json!({"positionsJson": blank, "positions": basket("INFY"), "symbol": "TCS"}),
        )
        .await;
        assert_eq!(positions, Some(json!([sent("INFY")])), "{:?}", blank);
    }
}

/// N-margin-07: a basket wins over the node's single position, even when
/// its symbol is set: only the basket is priced.
#[tokio::test]
async fn the_basket_wins_over_a_set_symbol() {
    let (positions, logs) = margin_sent(json!({"positionsJson": basket("SBIN"), "symbol": "TCS",
                                                "exchange": "BSE", "quantity": 5}))
    .await;
    assert_eq!(positions, Some(json!([sent("SBIN")])));
    assert!(
        logs.iter()
            .any(|(_, m)| m == "Calculating margin for a basket of 1 position(s)"),
        "{:?}",
        logs
    );
}

/// N-margin-03, decision 36: the margin basket refuses a price the order
/// nodes refuse, in the shared check's sentences, before any call.
#[tokio::test]
async fn a_basket_price_is_checked_with_the_order_nodes_sentences() {
    let cases = [
        (
            json!({"pricetype": "LIMIT"}),
            "Margin position 1: LIMIT order needs a positive price, got 0.0",
        ),
        (
            json!({"pricetype": "SL", "price": 101, "trigger_price": 0}),
            "Margin position 1: SL order needs a positive trigger price, got 0.0",
        ),
        (
            json!({"pricetype": "SL-M", "triggerPrice": -2.5}),
            "Margin position 1: SL-M order needs a positive trigger price, got -2.5",
        ),
    ];
    for (extra, sentence) in cases {
        let mut position = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY",
                                  "quantity": 1, "product": "MIS"});
        if let (Some(p), Some(e)) = (position.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        let client = Arc::new(FakeFlowClient::new());
        let c = ctx(client.clone());
        let out = run(
            &c,
            "margin",
            json!({"positionsJson": json!([position]).to_string()}),
        )
        .await;
        assert_eq!(
            out.value().cloned(),
            Some(json!({"status": "error", "message": sentence}))
        );
        assert!(
            logs(&c)
                .iter()
                .any(|(l, m)| l == "error" && *m == format!("Margin failed: {}", sentence)),
            "{:?}",
            logs(&c)
        );
        assert!(client.calls().is_empty());
    }
}

/// N-margin-03: the shared check's four sentences, word for word (Python's
/// `str(float)` for the number), and the prices it lets through.
#[test]
fn the_price_check_sentences_are_the_webs() {
    let nan = f64::NAN;
    let inf = f64::INFINITY;
    assert_eq!(
        invalid_price_reason("limit", inf, 0.0).as_deref(),
        Some("LIMIT order needs a finite price, got inf")
    );
    assert_eq!(
        invalid_price_reason("SL", 100.0, nan).as_deref(),
        Some("SL order needs a finite trigger price, got nan")
    );
    assert_eq!(
        invalid_price_reason("SL", 0.0, 99.0).as_deref(),
        Some("SL order needs a positive price, got 0.0")
    );
    assert_eq!(
        invalid_price_reason("SL-M", 0.0, -1.0).as_deref(),
        Some("SL-M order needs a positive trigger price, got -1.0")
    );
    assert_eq!(
        invalid_price_reason("", nan, 0.0).as_deref(),
        Some("MARKET order needs a finite price, got nan")
    );
    for (kind, price, trigger) in [
        ("MARKET", 0.0, 0.0),
        ("", 0.0, 0.0),
        ("LIMIT", 1.5, 0.0),
        ("SL", 101.0, 100.0),
        ("SL-M", 0.0, 100.0),
    ] {
        assert_eq!(invalid_price_reason(kind, price, trigger), None, "{}", kind);
    }
}
