//! The legacy `evaluate_trail` dict shape (web `services/risk/adapters.py`),
//! so a consumer can adopt the core without changing its persistence or its
//! Socket.IO payloads. Still pure.

use super::models::{value_to_f64, PositionRisk};
use super::position::evaluate_position;
use serde_json::{json, Value};

/// Drop-in replacement for the scalping monitor's `evaluate_trail`: same
/// input dict, same five output keys, same `sl` / `target` reasons.
pub fn evaluate_trail(state: &Value, last_price: &Value) -> Value {
    let risk = PositionRisk::from_state(state);
    let decision = evaluate_position(&risk, value_to_f64(last_price));
    if !decision.evaluated {
        // The legacy contract has no "not evaluated" state: report the input
        // unchanged and no breach.
        let get = |k: &str| state.get(k).cloned().unwrap_or(Value::Null);
        return json!({
            "highest_price": get("highest_price"),
            "lowest_price": get("lowest_price"),
            "current_sl": get("current_sl"),
            "breached": false,
            "reason": Value::Null,
        });
    }
    decision.to_trail_state()
}
