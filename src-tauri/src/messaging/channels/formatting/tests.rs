//! The shared alert formatter, and the WhatsApp text it must not change
//! (web `test/test_alerts_formatting.py`).
//!
//! `tests/fixtures/web/alerts/whatsapp_alert_golden.json` is the web's
//! fixture: the messages the old WhatsApp formatter produced for 24 order
//! events, recorded with the clock pinned. A trader on WhatsApp must
//! receive exactly the same text. GTT orders are the one deliberate change.

use super::*;
use crate::messaging::channels::bots::render_whatsapp;
use serde_json::json;

const RECORDED_AT: &str = "09:15:30";

fn golden() -> Vec<Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/web/alerts/whatsapp_alert_golden.json");
    let text = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&text).unwrap()
}

const GTT_TYPES: [&str; 5] = [
    "placegttorder",
    "modifygttorder",
    "cancelgttorder",
    "gtttriggered",
    "gttexpired",
];

/// The mode the web's formatter reads: `response.get("mode", "live")`.
fn analyze(response: &Value) -> bool {
    response.get("mode").and_then(Value::as_str) == Some("analyze")
}

fn build(api_type: &str, request: Value, response: Value) -> AlertMessage {
    build_message(
        api_type,
        &request,
        &response,
        analyze(&response),
        RECORDED_AT,
    )
}

/// Web test/test_alerts_formatting.py::test_whatsapp_text_is_unchanged
#[test]
fn whatsapp_text_is_unchanged() {
    let cases = golden();
    assert_eq!(cases.len(), 24);
    for case in cases {
        let api_type = case["api_type"].as_str().unwrap();
        if GTT_TYPES.contains(&api_type) {
            continue;
        }
        let message = build(api_type, case["request"].clone(), case["response"].clone());
        assert_eq!(
            render_whatsapp(&message),
            case["message"].as_str().unwrap(),
            "{}",
            case
        );
    }
}

/// Web test/test_alerts_formatting.py::test_the_golden_corpus_covers_every_titled_order_type
#[test]
fn the_golden_corpus_covers_every_titled_order_type() {
    let covered: Vec<String> = golden()
        .iter()
        .map(|c| c["api_type"].as_str().unwrap().to_string())
        .collect();
    for api_type in [
        "placeorder",
        "placesmartorder",
        "basketorder",
        "splitorder",
        "optionsorder",
        "optionsmultiorder",
        "modifyorder",
        "cancelorder",
        "cancelallorder",
        "closeposition",
    ] {
        assert!(covered.iter().any(|c| c == api_type), "{}", api_type);
    }
}

/// Web test/test_alerts_formatting.py::test_a_gtt_alert_used_to_say_nothing_and_now_says_what_it_is
#[test]
fn a_gtt_alert_used_to_say_nothing_and_now_says_what_it_is() {
    let old = golden()
        .into_iter()
        .find(|c| c["api_type"] == "placegttorder")
        .unwrap();
    assert!(old["message"]
        .as_str()
        .unwrap()
        .starts_with("*Order Update*"));
    let message = build(
        "placegttorder",
        json!({"symbol": "SBIN", "action": "BUY", "quantity": 10, "exchange": "NSE",
            "trigger_type": "OCO", "triggerprice_sl": 790.0, "triggerprice_tg": 850.0}),
        json!({"status": "success", "trigger_id": "G1", "mode": "live"}),
    );
    assert_eq!(message.title, "GTT Order Placed");
    assert_eq!(
        message.lines,
        [
            "Symbol: SBIN",
            "Action: BUY",
            "Quantity: 10",
            "Exchange: NSE",
            "Trigger Type: OCO",
            "Stop Loss Trigger: 790.0",
            "Target Trigger: 850.0",
            "Trigger ID: G1",
            "Time: 09:15:30",
        ]
    );
}

/// Web test/test_alerts_formatting.py::test_a_single_gtt_shows_only_its_own_trigger
#[test]
fn a_single_gtt_shows_only_its_own_trigger() {
    let message = build(
        "placegttorder",
        json!({"symbol": "SBIN", "trigger_type": "SINGLE", "triggerprice_tg": 850.0}),
        json!({"status": "error", "message": "Price out of range"}),
    );
    assert!(message.lines.iter().any(|l| l == "Target Trigger: 850.0"));
    assert!(!message
        .lines
        .iter()
        .any(|l| l.starts_with("Stop Loss Trigger")));
    assert!(message
        .lines
        .iter()
        .any(|l| l == "Error: Price out of range"));
}

/// Web test/test_alerts_formatting.py::test_every_gtt_event_has_its_title_and_fields
#[test]
fn every_gtt_event_has_its_title_and_fields() {
    let cases = [
        (
            "modifygttorder",
            json!({"trigger_id": "G1", "symbol": "SBIN", "triggerprice_sl": 780.0}),
            json!({"status": "success"}),
            vec![
                "Trigger ID: G1",
                "Stop Loss Trigger: 780.0",
                "Modification Successful",
            ],
            "GTT Order Modified",
        ),
        (
            "cancelgttorder",
            json!({"trigger_id": "G1"}),
            json!({"status": "success"}),
            vec!["Trigger ID: G1", "Cancellation Successful"],
            "GTT Order Cancelled",
        ),
        (
            "gtttriggered",
            json!({"symbol": "SBIN", "exchange": "NSE", "trigger_id": "G1", "trigger_prices": [790.0]}),
            json!({"status": "success", "orderid": "77"}),
            vec!["Trigger ID: G1", "Trigger Prices: 790.0", "Order ID: 77"],
            "GTT Triggered",
        ),
        (
            "gttexpired",
            json!({"symbol": "SBIN", "exchange": "NSE", "trigger_id": "G1"}),
            json!({"status": "success", "message": "GTT expired without firing"}),
            vec!["Trigger ID: G1", "GTT expired without firing"],
            "GTT Expired",
        ),
    ];
    for (api_type, request, response, expected, title) in cases {
        let message = build(api_type, request, response);
        assert_eq!(message.title, title);
        for line in expected {
            assert!(
                message.lines.iter().any(|l| l == line),
                "{}: {} in {:?}",
                api_type,
                line,
                message.lines
            );
        }
    }
}

/// Web test/test_alerts_formatting.py::test_an_unreadable_event_still_raises_an_alert
#[test]
fn an_unreadable_event_still_raises_an_alert() {
    let message = build(
        "basketorder",
        json!({}),
        json!({"status": "success", "results": [null]}),
    );
    assert_eq!(
        message,
        AlertMessage {
            title: "Basket Order Executed",
            lines: vec!["Order Type: basketorder".into(), "Status: success".into()],
            banner: None,
            strategy: None,
        }
    );
    // Results that are not a list, and trigger prices that are not one.
    let split = build("splitorder", json!({}), json!({"results": {"a": 1}}));
    assert_eq!(split.banner, None);
    let fired = build(
        "gtttriggered",
        json!({"trigger_prices": 790}),
        json!({"status": "success"}),
    );
    assert_eq!(fired.banner, None);
}

/// Web test/test_alerts_formatting.py::test_the_mode_banner_follows_the_response
#[test]
fn the_mode_banner_follows_the_mode() {
    let live = build(
        "placeorder",
        json!({}),
        json!({"status": "success", "mode": "live"}),
    );
    let analyzed = build(
        "placeorder",
        json!({}),
        json!({"status": "success", "mode": "analyze"}),
    );
    let unmarked = build("placeorder", json!({}), json!({"status": "success"}));
    assert_eq!(live.banner, Some(LIVE_BANNER));
    assert_eq!(analyzed.banner, Some(ANALYZE_BANNER));
    assert_eq!(unmarked.banner, Some(LIVE_BANNER));
}

/// Web test/test_alerts_formatting.py::test_the_time_is_the_last_line
#[test]
fn the_time_is_the_last_line() {
    let message = build(
        "cancelorder",
        json!({"orderid": "1"}),
        json!({"status": "success"}),
    );
    assert_eq!(message.lines.last().unwrap(), "Time: 09:15:30");
}

/// A broker that lists cancelled order ids as numbers still gets them
/// shown (the web falls back to the short message there).
#[test]
fn numeric_cancelled_order_ids_are_listed() {
    let message = build(
        "cancelallorder",
        json!({}),
        json!({"status": "success", "canceled_orders": [11, 12], "failed_cancellations": []}),
    );
    assert!(message.lines.iter().any(|l| l == "Order IDs: 11, 12"));
}
