//! The order tools' argument checks (the web's `OrdersToolkit._build_order`
//! and its validators), in the web's order and words.
//!
//! Every failure is a fixable input error the model reads as a correction
//! ([`ToolOutcome::Retry`], the web's `RetryAgentRun`), raised before
//! anything is checked by the guard or sent. The guard checks it all again
//! afterwards and is the authority; this exists so the model gets a
//! sentence it can act on.

use super::AGENT_STRATEGY;
use crate::agent::contract::tool::ToolOutcome;
use crate::agent::safety::risk::num::{py_decimal, py_g, py_repr, py_str};
use crate::agent::tools::order_vocab::{
    canonical_action, canonical_price_type, canonical_product, split_exchange_prefix,
    VALID_ACTIONS, VALID_PRICE_TYPES, VALID_PRODUCT_TYPES,
};
use crate::services::schemas::VALID_EXCHANGES;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

pub(super) type Check<T> = Result<T, ToolOutcome>;

/// Price types that must carry a limit price, and those that must carry a
/// trigger; any other must carry neither.
const NEEDS_PRICE: [&str; 2] = ["LIMIT", "SL"];
const NEEDS_TRIGGER: [&str; 2] = ["SL", "SL-M"];

/// Quote-only index codes and where their contracts trade.
const INDEX_TRADING_VENUES: [(&str, Option<&str>); 4] = [
    ("NSE_INDEX", Some("NFO")),
    ("BSE_INDEX", Some("BFO")),
    ("MCX_INDEX", Some("MCX")),
    ("GLOBAL_INDEX", None),
];

static NULL: Value = Value::Null;

pub(super) fn arg<'a>(args: &'a Map<String, Value>, key: &str) -> &'a Value {
    args.get(key).unwrap_or(&NULL)
}

/// Python's `str(value)`, or `None` for a missing value.
fn text_of(v: &Value) -> Option<String> {
    (!v.is_null()).then(|| py_str(v))
}

/// Why an order on a quote-only index code cannot be placed, naming the
/// venue its contracts trade on; `None` for any other exchange.
pub fn index_refusal(exchange: &str) -> Option<String> {
    let (_, venue) = INDEX_TRADING_VENUES.iter().find(|(code, _)| *code == exchange)?;
    Some(match venue {
        None => format!(
            "{} is a quote-only code for a global index, and there is no contract for it that this platform can trade.",
            exchange
        ),
        Some(venue) => format!(
            "{} is a quote-only index code: an index itself cannot be bought or sold. Trade its future or one of its options on {} instead, with the exact symbol the symbol search or option chain tool returns, and state the new contract to the operator before placing it.",
            exchange, venue
        ),
    })
}

/// A clean refusal from the broker or the platform, worded so it agrees
/// with `retry` being false: a corrected order is a new order needing a new
/// approval.
pub fn refusal_message(detail: &str) -> String {
    let reason = if detail.is_empty() {
        "No reason was given.".to_string()
    } else {
        format!("The reason given was: {}.", detail.trim_end_matches('.'))
    };
    format!(
        "The broker or the platform refused this request, so nothing was placed or changed. {} Do not send it again on your own. Tell the operator the reason in plain words. If it names something that can be corrected, such as a price off the tick size, a quantity that is not a whole number of lots or a product the segment does not allow, say what you would change and send a corrected request only if they ask, because it needs their approval again.",
        reason
    )
}

pub(super) fn required_text(field: &str, value: &Value) -> Check<String> {
    let text = text_of(value).unwrap_or_default().trim().to_string();
    if text.is_empty() {
        return Err(ToolOutcome::invalid_argument(
            field,
            "it was empty.",
            Some("Pass the value the operator gave you, or look it up with the symbol search tool first."),
        ));
    }
    Ok(text)
}

/// A broker order id: a number is taken as its text, a boolean is not an id.
pub(super) fn order_id(value: &Value) -> Check<String> {
    let text = match value {
        Value::Null | Value::Bool(_) => String::new(),
        v => py_str(v).trim().to_string(),
    };
    if text.is_empty() {
        return Err(ToolOutcome::invalid_argument(
            "order_id",
            "it was empty, and an order cannot be identified without it.",
            Some("Read the exact order id from the order book and pass it verbatim."),
        ));
    }
    Ok(text)
}

pub(super) fn exchange(value: &Value) -> Check<String> {
    let text = required_text("exchange", value)?.to_uppercase();
    if let Some(refusal) = index_refusal(&text) {
        return Err(ToolOutcome::invalid_argument("exchange", &refusal, None));
    }
    if !VALID_EXCHANGES.contains(&text.as_str()) {
        return Err(ToolOutcome::invalid_argument(
            "exchange",
            &format!("{} is not an OpenAlgo exchange code.", text),
            Some(&format!("Use one of: {}.", VALID_EXCHANGES.join(", "))),
        ));
    }
    Ok(text)
}

fn action(value: &Value) -> Check<String> {
    let text = canonical_action(Some(&required_text("action", value)?));
    if !VALID_ACTIONS.contains(&text.as_str()) {
        return Err(ToolOutcome::invalid_argument(
            "action",
            &format!("{} is not a side.", text),
            Some(&format!("Use one of: {}.", VALID_ACTIONS.join(", "))),
        ));
    }
    Ok(text)
}

pub(super) fn product(value: &Value) -> Check<String> {
    let text = canonical_product(Some(&required_text("product", value)?));
    if !VALID_PRODUCT_TYPES.contains(&text.as_str()) {
        return Err(ToolOutcome::invalid_argument(
            "product",
            &format!("{} is not a product type.", text),
            Some(&format!(
                "Use one of: {}. CNC is delivery in a cash segment, NRML carries a derivative overnight, MIS is intraday.",
                VALID_PRODUCT_TYPES.join(", ")
            )),
        ));
    }
    Ok(text)
}

fn price_type(value: &Value) -> Check<String> {
    let text = canonical_price_type(Some(&required_text("price_type", value)?));
    if !VALID_PRICE_TYPES.contains(&text.as_str()) {
        return Err(ToolOutcome::invalid_argument(
            "price_type",
            &format!("{} is not a price type.", text),
            Some(&format!("Use one of: {}.", VALID_PRICE_TYPES.join(", "))),
        ));
    }
    Ok(text)
}

/// A whole number of units; `minimum` is `None` for a signed target.
pub(super) fn whole_number(field: &str, value: &Value, minimum: Option<i64>) -> Check<i64> {
    let Some(parsed) = py_decimal(value) else {
        return Err(ToolOutcome::invalid_argument(
            field,
            &format!("{} is not a number.", py_repr(value)),
            Some("Pass a whole number of units, for example 10."),
        ));
    };
    let Some(number) = (parsed == parsed.trunc()).then(|| parsed.to_i64()).flatten() else {
        return Err(ToolOutcome::invalid_argument(
            field,
            &format!("{} is not a whole number, and quantities are in units rather than fractions.", parsed),
            Some("Round to a whole number of units. For a derivative it must also be a multiple of the contract's lot size, which you look up rather than assume."),
        ));
    };
    if let Some(min) = minimum.filter(|m| number < *m) {
        return Err(ToolOutcome::invalid_argument(
            field,
            &format!("{} is below the smallest permitted value of {}.", number, min),
            Some("Quantity is a positive whole number of units, never lots and never zero."),
        ));
    }
    Ok(number)
}

/// A price in rupees, zero or positive; null or empty reads as zero.
fn money(field: &str, value: &Value) -> Check<f64> {
    if value.is_null() || value == &json!("") {
        return Ok(0.0);
    }
    let Some(parsed) = py_decimal(value) else {
        return Err(ToolOutcome::invalid_argument(
            field,
            &format!("{} is not a number.", py_repr(value)),
            Some("Pass a price in rupees, for example 1450.5, or 0 when the order type does not use it."),
        ));
    };
    if parsed < Decimal::ZERO {
        return Err(ToolOutcome::invalid_argument(
            field,
            &format!("{} is negative.", parsed),
            Some("A price is zero or positive. Use 0 when the order type does not use it."),
        ));
    }
    Ok(parsed.to_string().parse().unwrap_or(0.0))
}

/// Price and trigger against the price type: a priced type needs its
/// price, and a price a type ignores is refused rather than silently
/// zeroed, because the trader approved the arguments as written.
fn prices(kind: &str, args: &Map<String, Value>) -> Check<(f64, f64)> {
    let limit_price = money("price", arg(args, "price"))?;
    let trigger = money("trigger_price", arg(args, "trigger_price"))?;
    if NEEDS_PRICE.contains(&kind) && limit_price <= 0.0 {
        return Err(ToolOutcome::invalid_argument(
            "price",
            &format!("a {} order is priced by the operator, so it needs a limit price.", kind),
            Some("Pass the price in rupees, for example price=1450.5, or use MARKET if you meant to trade at whatever is available."),
        ));
    }
    if NEEDS_TRIGGER.contains(&kind) && trigger <= 0.0 {
        return Err(ToolOutcome::invalid_argument(
            "trigger_price",
            &format!("a {} order only becomes live once the market reaches its trigger, so it needs one.", kind),
            Some("Pass the trigger in rupees, for example trigger_price=1440.0."),
        ));
    }
    if !NEEDS_PRICE.contains(&kind) && limit_price > 0.0 {
        return Err(ToolOutcome::invalid_argument(
            "price",
            &format!("a {} order carries no limit price, yet price={} was sent, so the order the operator approved does not say which of the two they meant.", kind, py_g(limit_price)),
            Some(&format!("Drop price (or pass 0) to keep it a {} order, or switch price_type to LIMIT to trade at that price. Restate the corrected order to the operator, since it needs their approval again.", kind)),
        ));
    }
    if !NEEDS_TRIGGER.contains(&kind) && trigger > 0.0 {
        return Err(ToolOutcome::invalid_argument(
            "trigger_price",
            &format!("a {} order carries no trigger, yet trigger_price={} was sent, so the order the operator approved does not say whether they wanted a stop.", kind, py_g(trigger)),
            Some(&format!("Drop trigger_price (or pass 0) to keep it a {} order, or switch price_type to SL or SL-M for a stop order. Restate the corrected order to the operator, since it needs their approval again.", kind)),
        ));
    }
    Ok((limit_price, trigger))
}

/// The service payload in the service's own field names, tagged with the
/// Agent's strategy and carrying no credential (the web's `_build_order`).
pub(super) fn build_order(args: &Map<String, Value>, allow_zero_quantity: bool) -> Check<Map<String, Value>> {
    let kind = price_type(arg(args, "price_type"))?;
    let (limit_price, trigger) = prices(&kind, args)?;
    let (bare, venue) = split_exchange_prefix(
        text_of(arg(args, "symbol")).as_deref(),
        text_of(arg(args, "exchange")).as_deref(),
    )?;
    let mut order = Map::new();
    order.insert("strategy".into(), json!(AGENT_STRATEGY));
    order.insert("symbol".into(), json!(required_text("symbol", &json!(bare))?.to_uppercase()));
    order.insert("exchange".into(), json!(exchange(&json!(venue))?));
    order.insert("action".into(), json!(action(arg(args, "action"))?));
    let minimum = if allow_zero_quantity { 0 } else { 1 };
    order.insert("quantity".into(), json!(whole_number("quantity", arg(args, "quantity"), Some(minimum))?));
    order.insert("product".into(), json!(product(arg(args, "product"))?));
    order.insert("pricetype".into(), json!(kind));
    order.insert("price".into(), json!(limit_price));
    order.insert("trigger_price".into(), json!(trigger));
    order.insert("disclosed_quantity".into(), json!(0));
    Ok(order)
}

#[cfg(test)]
mod tests;
