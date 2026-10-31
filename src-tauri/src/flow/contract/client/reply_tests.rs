//! The web client's reply rules the shape tests leave open (parity D-03,
//! CL-15): when an answer counts as a success, and what a matched position
//! answers when its row lacks a field. The dicts are the web's
//! `_handle_response` and `get_open_position` (the position cases checked
//! in its Python); success is decided from the HTTP status and the body as
//! D-03 records.

use super::fake::FakeFlowClient;
use super::*;

/// D-03: success needs a 2xx status AND a body whose `status` is not
/// "error". A 2xx body that says error is a failure carrying its message
/// and that status as its code; a body with no status passes through; a
/// non-2xx fails whatever its body says.
#[test]
fn a_2xx_answer_whose_body_says_error_is_a_failure() {
    assert_eq!(
        handle_response(
            200,
            &json!({"status": "error", "message": "Insufficient funds"})
        ),
        json!({"status": "error", "error": "Insufficient funds", "code": 200})
    );
    assert_eq!(
        handle_response(201, &json!({"status": "error"})),
        json!({"status": "error", "error": "Unknown error", "code": 201})
    );
    assert_eq!(
        handle_response(200, &json!({"orderid": "42"})),
        json!({"status": "success", "orderid": "42"})
    );
    assert_eq!(
        handle_response(299, &json!({"status": "success", "data": 1})),
        json!({"status": "success", "data": 1})
    );
    assert_eq!(
        handle_response(400, &json!({"status": "success", "message": "Bad lot"})),
        json!({"status": "error", "error": "Bad lot", "code": 400})
    );
    assert_eq!(
        handle_response(300, &json!({"status": "success"})),
        json!({"status": "error", "error": "Unknown error", "code": 300})
    );
}

/// CL-15: the first row matching symbol and exchange answers, and a field
/// it lacks answers 0 (Python's `pos.get("quantity", 0)`); the row is not
/// skipped for it. A field present as null stays null, and a text value
/// keeps its type, as the web's dict does.
#[tokio::test]
async fn a_matched_row_lacking_quantity_or_pnl_answers_zero() {
    let c = FakeFlowClient::new();
    c.reply(
        "positionbook",
        json!({"status": "success", "data": [
            {"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "pnl": 4.5},
            {"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": 7, "pnl": 1.0},
            {"symbol": "INFY", "exchange": "NSE", "product": "MIS", "quantity": -2},
            {"symbol": "TCS", "exchange": "NSE", "quantity": null, "pnl": null},
            {"symbol": "HDFC", "exchange": "NSE", "quantity": "5", "pnl": "1.5"}
        ]}),
    );
    for product in [Some("MIS"), None] {
        assert_eq!(
            c.get_open_position("SBIN", "NSE", product).await,
            json!({"status": "success", "quantity": 0, "pnl": 4.5}),
            "{:?}",
            product
        );
    }
    assert_eq!(
        c.get_open_position("INFY", "NSE", None).await,
        json!({"status": "success", "quantity": -2, "pnl": 0})
    );
    assert_eq!(
        c.get_open_position("TCS", "NSE", None).await,
        json!({"status": "success", "quantity": null, "pnl": null})
    );
    assert_eq!(
        c.get_open_position("HDFC", "NSE", None).await,
        json!({"status": "success", "quantity": "5", "pnl": "1.5"})
    );
}
