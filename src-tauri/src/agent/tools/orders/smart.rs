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
//!
//! Then the order that was checked is the order sent (lead decision,
//! 2026-10-27): the position is read again at dispatch and, unchanged, a
//! plain `placeorder` of the checked side and size goes out, never
//! `placesmartorder`, which would decide again from whatever is held by
//! then. A position that moved since the check (or since the approval
//! card showed it) sends nothing. The web sends
//! `place_smart_order_service`; in Semi-Auto the Action Center here shows
//! the fixed order that was approved.

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

/// The identity of one smart order for its approval card's record: the
/// checked order's symbol, exchange, product, target, quantity and side,
/// as the tool and the card's preview both read them.
pub(super) fn order_key(
    order: &serde_json::Map<String, Value>,
    target: i64,
    quantity: i64,
) -> String {
    let text = |k: &str| {
        order
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    [
        text("symbol"),
        text("exchange"),
        text("product"),
        target.to_string(),
        quantity.to_string(),
        text("action"),
    ]
    .join("|")
}

/// What is held, in a trader's words.
fn holding(n: i64) -> String {
    match n {
        n if n > 0 => format!("long {}", n),
        n if n < 0 => format!("short {}", -n),
        _ => "flat".into(),
    }
}

/// The refusal when the position moved between what was shown or checked
/// and what is held now.
pub(super) fn position_changed(symbol: &str, from: i64, to: i64) -> String {
    format!(
        "Your position in {} changed from {} to {} since this was checked. Nothing was sent; ask again.",
        symbol,
        holding(from),
        holding(to)
    )
}

/// What to do at dispatch, from the position held now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AtDispatch {
    /// The position moved, or could not be read: send nothing.
    Refuse(String),
    /// Already where the order would take it: send nothing.
    Nothing(&'static str),
    /// Send the checked order.
    Send { action: String, quantity: i64 },
}

/// The checked order holds only while the position it was sized from is
/// still the one held.
pub(super) fn at_dispatch(
    names: (&str, &str, &str),
    checked: i64,
    now: Option<i64>,
    sizes: (i64, i64),
    action: &str,
) -> AtDispatch {
    let (symbol, exchange, product) = names;
    let Some(now) = now else {
        return AtDispatch::Refuse(position_unreadable(symbol, exchange, product));
    };
    if now != checked {
        return AtDispatch::Refuse(position_changed(symbol, checked, now));
    }
    match decide(now, sizes.0, sizes.1, action) {
        SmartDecision::Place { action, quantity } => AtDispatch::Send { action, quantity },
        SmartDecision::NoAction(message) => AtDispatch::Nothing(message),
    }
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

    #[test]
    fn the_checked_order_is_sent_only_while_the_position_is_unchanged() {
        let names = ("SBIN", "NSE", "MIS");
        assert_eq!(
            at_dispatch(names, 500, Some(500), (-500, 0), "BUY"),
            AtDispatch::Send {
                action: "SELL".into(),
                quantity: 1000
            }
        );
        assert_eq!(
            at_dispatch(names, 100, Some(150), (150, 0), "BUY"),
            AtDispatch::Refuse(
                "Your position in SBIN changed from long 100 to long 150 since this was checked. Nothing was sent; ask again.".into()
            )
        );
        assert_eq!(
            at_dispatch(names, -20, Some(0), (40, 0), "BUY"),
            AtDispatch::Refuse(
                "Your position in SBIN changed from short 20 to flat since this was checked. Nothing was sent; ask again.".into()
            )
        );
        let AtDispatch::Refuse(unread) = at_dispatch(names, 100, None, (150, 0), "BUY") else {
            panic!()
        };
        assert!(unread.starts_with("The position held in SBIN on NSE under MIS could not be read"));
        assert_eq!(
            at_dispatch(names, 0, Some(0), (0, 0), "BUY"),
            AtDispatch::Nothing("No OpenPosition Found. Not placing Exit order.")
        );
    }
}
