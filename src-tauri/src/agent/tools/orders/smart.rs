//! What a smart order will actually send (security review, 2026-10-27).
//!
//! A smart order moves the position held to `position_size`, so the order
//! the platform sends is the difference, not the target: holding long 500
//! and asking for short 500 sends SELL 1,000. The web's tool checked the
//! larger of the target and the quantity (500 here), so a reversal could
//! pass the quantity and notional caps at half its real size. The desktop
//! reads the position held first, decides the order with the platform's
//! own smart-order table (`services::order_service::smart_decision`, the
//! one the service runs), and the guard checks that order: its quantity
//! and its side, for every rule and the duplicate window. A position that
//! cannot be read is refused, never guessed.

use crate::services::order_service::{smart_decision, SmartDecision};
use serde_json::{json, Value};

/// The order a smart order sends from `held`, or why it sends none.
pub(super) fn decide(held: i64, target: i64, quantity: i64, action: &str) -> SmartDecision {
    smart_decision(held, target, quantity, action)
}

/// What the trader is told when the position cannot be read.
pub(super) fn position_unreadable(symbol: &str, exchange: &str, product: &str) -> String {
    format!(
        "The position held in {} on {} under {} could not be read, so the order this smart order would send is not known, and nothing was sent. Tell the operator the position could not be read, and ask again once the positions are available.",
        symbol, exchange, product
    )
}

/// The decision as data for the approval card and the result: what is
/// held, the target, and the order it takes (none when already there).
pub fn describe(held: i64, target: i64, quantity: i64, action: &str) -> Value {
    match decide(held, target, quantity, action) {
        SmartDecision::Place { action, quantity } => json!({
            "held": held, "target": target, "action": action, "quantity": quantity,
        }),
        SmartDecision::NoAction(message) => json!({
            "held": held, "target": target, "action": null, "quantity": 0, "message": message,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sends(held: i64, target: i64) -> (String, i64) {
        match decide(held, target, 0, "BUY") {
            SmartDecision::Place { action, quantity } => (action, quantity),
            SmartDecision::NoAction(m) => (m.to_string(), 0),
        }
    }

    #[test]
    fn the_order_sent_is_the_difference_from_what_is_held() {
        assert_eq!(sends(500, -500), ("SELL".into(), 1000), "a reversal");
        assert_eq!(sends(100, 150), ("BUY".into(), 50), "an increase");
        assert_eq!(sends(100, 40), ("SELL".into(), 60), "a reduction");
        assert_eq!(sends(-75, 0), ("BUY".into(), 75), "a short covered");
        assert_eq!(sends(0, -30), ("SELL".into(), 30), "a new short");
        let (message, quantity) = sends(100, 100);
        assert_eq!(
            (message.as_str(), quantity),
            ("No OpenPosition Found. Not placing Exit order.", 0)
        );
        assert_eq!(
            describe(500, -500, 0, "BUY"),
            json!({"held": 500, "target": -500, "action": "SELL", "quantity": 1000})
        );
        assert_eq!(
            describe(100, 100, 5, "BUY")["message"],
            "Positions Already Matched. No Action needed."
        );
    }
}
