//! Order vocabulary shared by the order tools and the read-only account
//! tools (the web's `services/agent/tools/order_vocab.py`), kept apart
//! from `orders` so a read-only toolkit never depends on the mutating one.
//!
//! Each alias names exactly one OpenAlgo code, so reading one cannot change
//! what the trader approved; anything else is upper-cased and left for the
//! caller to validate.

use crate::agent::contract::tool::ToolOutcome;

pub const VALID_PRODUCT_TYPES: [&str; 3] = ["CNC", "NRML", "MIS"];
pub const VALID_PRICE_TYPES: [&str; 4] = ["MARKET", "LIMIT", "SL", "SL-M"];
pub const VALID_ACTIONS: [&str; 2] = ["BUY", "SELL"];

/// Other spellings of a product.
const PRODUCT_ALIASES: [(&str, &str); 5] = [
    ("INTRADAY", "MIS"),
    ("DELIVERY", "CNC"),
    ("CARRYFORWARD", "NRML"),
    ("CARRY_FORWARD", "NRML"),
    ("NORMAL", "NRML"),
];

/// Other spellings of a price type, keyed after spaces, hyphens and
/// underscores are folded to one underscore.
const PRICE_TYPE_ALIASES: [(&str, &str); 8] = [
    ("SL_M", "SL-M"),
    ("SLM", "SL-M"),
    ("STOPLOSS_MARKET", "SL-M"),
    ("STOP_LOSS_MARKET", "SL-M"),
    ("STOPLOSS", "SL"),
    ("STOP_LOSS", "SL"),
    ("STOPLOSS_LIMIT", "SL"),
    ("STOP_LOSS_LIMIT", "SL"),
];

const ACTION_ALIASES: [(&str, &str); 2] = [("B", "BUY"), ("S", "SELL")];

fn alias(table: &[(&str, &str)], key: &str) -> Option<String> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
}

fn upper(value: Option<&str>) -> String {
    value.unwrap_or_default().trim().to_uppercase()
}

/// `INTRADAY` to `MIS`, `DELIVERY` to `CNC`, the carry-forward spellings to
/// `NRML`; anything else upper-cased.
pub fn canonical_product(value: Option<&str>) -> String {
    let text = upper(value);
    alias(&PRODUCT_ALIASES, &text).unwrap_or(text)
}

/// `SL M`, `sl_m`, `STOP-LOSS-MARKET` to `SL-M`; the stop-loss limit
/// spellings to `SL`; anything else upper-cased.
pub fn canonical_price_type(value: Option<&str>) -> String {
    let text = upper(value);
    let mut folded = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_whitespace() || c == '_' || c == '-' {
            if !folded.ends_with('_') {
                folded.push('_');
            }
        } else {
            folded.push(c);
        }
    }
    alias(&PRICE_TYPE_ALIASES, &folded).unwrap_or(text)
}

/// `B` to `BUY`, `S` to `SELL`; anything else upper-cased.
pub fn canonical_action(value: Option<&str>) -> String {
    let text = upper(value);
    alias(&ACTION_ALIASES, &text).unwrap_or(text)
}

/// Separate `EXCHANGE:SYMBOL`. The prefix fills an empty exchange or one
/// that names the same exchange; when the two disagree the call is refused
/// rather than guessed, because either guess could trade an instrument the
/// trader did not approve.
pub fn split_exchange_prefix(
    symbol: Option<&str>,
    exchange: Option<&str>,
) -> Result<(String, String), ToolOutcome> {
    let text = upper(symbol);
    let venue = upper(exchange);
    let Some((prefix, bare)) = text.split_once(':') else {
        return Ok((text, venue));
    };
    let (prefix, bare) = (prefix.trim(), bare.trim());
    if !venue.is_empty() && venue != prefix {
        return Err(ToolOutcome::invalid_argument(
            "symbol",
            &format!(
                "it is written as {}, which names {}, but the exchange argument is {}, and the two disagree.",
                text, prefix, venue
            ),
            Some(&format!(
                "Pass the bare symbol {} with the exchange the operator meant, and say which one you used.",
                bare
            )),
        ));
    }
    Ok((bare.to_string(), prefix.to_string()))
}
