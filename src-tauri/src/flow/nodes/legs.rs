//! The legs of a multi-leg options order (the web's
//! `execute_options_multi_order` leg handling and `_generate_strategy_legs`,
//! `flow_executor_service.py` 1234-1561): custom legs checked one by one
//! before anything is sent (a multi-leg basket fills leg by leg, so a leg
//! refused mid-basket would leave half a position on), and the readymade
//! strategies' legs.

use super::orders::{invalid_price_reason, VALID_ACTIONS, VALID_PRICE_TYPES, VALID_PRODUCT_TYPES};
use crate::flow::contract::node::{NodeCtx, WARNING};
use crate::flow::contract::pyfmt;
use crate::flow::contract::resolve::OrderFields;
use crate::flow::node_contracts::{
    is_expiry_date, is_option_offset, VALID_EXPIRY_TYPES, VALID_LEG_STRIKE_MODES,
};
use serde_json::{json, Map, Value};

/// Option types a leg may name.
pub const OPTION_TYPES: [&str; 2] = ["CE", "PE"];

/// The node's values a custom leg inherits unless it sets its own.
pub struct LegDefaults {
    pub product: String,
    pub price_type: String,
    pub price: f64,
    pub trigger_price: f64,
}

/// How a custom leg names its strike.
#[derive(Debug, Clone, PartialEq)]
pub enum Strike {
    /// ATM, ITM1-ITM50 or OTM1-OTM50, re-resolved against the live
    /// underlying on every run.
    Offset(String),
    /// One listed strike, used as given.
    Absolute(f64),
}

/// One custom leg, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomLeg {
    pub strike: Strike,
    /// Its own DDMMMYY expiry, or empty.
    pub expiry_date: String,
    /// Its own relative expiry, or empty.
    pub expiry_type: String,
    pub option_type: String,
    pub action: String,
    pub quantity_lots: i64,
    pub pricetype: String,
    pub product: String,
    pub price: f64,
    pub trigger_price: f64,
    pub splitsize: i64,
}

impl CustomLeg {
    /// The leg the service receives: `quantity` in contracts, exactly one
    /// strike selector, and `expiry_date` only when the leg has its own
    /// (an absent key inherits the basket's).
    pub fn payload(&self, lot_size: i64, expiry: &str) -> Value {
        let mut m = Map::new();
        match &self.strike {
            Strike::Offset(o) => m.insert("offset".into(), json!(o)),
            Strike::Absolute(s) => m.insert("strike".into(), json!(s)),
        };
        m.insert("option_type".into(), json!(self.option_type));
        m.insert("action".into(), json!(self.action));
        m.insert("pricetype".into(), json!(self.pricetype));
        m.insert("product".into(), json!(self.product));
        m.insert("price".into(), json!(self.price));
        m.insert("trigger_price".into(), json!(self.trigger_price));
        m.insert("splitsize".into(), json!(self.splitsize));
        m.insert(
            "quantity".into(),
            json!(self.quantity_lots.saturating_mul(lot_size)),
        );
        if !expiry.is_empty() {
            m.insert("expiry_date".into(), json!(expiry));
        }
        Value::Object(m)
    }
}

/// The custom legs of a node (`legs`, or the legacy `orderLegs`, which may
/// be a `{{var}}` holding the list), each checked in the web's order; the
/// first fault is reported as `leg {n}: ...`.
pub fn custom_legs(
    ctx: &NodeCtx,
    f: &OrderFields<'_>,
    defaults: &LegDefaults,
) -> Result<Vec<CustomLeg>, String> {
    let raw = f.value("legs", None, &["orderLegs"])?;
    let legs = match raw {
        Value::Array(items) if !items.is_empty() => items,
        _ => return Err("legs must resolve to a non-empty list of objects".into()),
    };
    if legs.iter().any(|l| !l.is_object()) {
        return Err("every resolved leg must be an object".into());
    }
    legs.iter()
        .filter_map(Value::as_object)
        .enumerate()
        .map(|(n, leg)| {
            custom_leg(&OrderFields::new(&ctx.vars, leg), defaults)
                .map_err(|e| format!("leg {}: {}", n + 1, e))
        })
        .collect()
}

fn custom_leg(f: &OrderFields<'_>, d: &LegDefaults) -> Result<CustomLeg, String> {
    let strike = match f
        .choice("strikeMode", &VALID_LEG_STRIKE_MODES, Some("OFFSET"), &[])?
        .as_str()
    {
        "STRIKE" => {
            let s = f.number("strike", None, &[])?;
            if s <= 0.0 {
                return Err(format!(
                    "strike must be a positive number, got {}",
                    pyfmt::float(s)
                ));
            }
            Strike::Absolute(s)
        }
        _ => {
            let o = f.text("offset", None, &[])?.to_uppercase();
            if !is_option_offset(&o) {
                return Err(format!(
                    "offset must be ATM, ITM1-ITM50, or OTM1-OTM50, got {}",
                    pyfmt::quote(&o)
                ));
            }
            Strike::Offset(o)
        }
    };
    let expiry_date = f.optional_text("expiry")?.to_uppercase();
    if !expiry_date.is_empty() && !is_expiry_date(&expiry_date) {
        return Err(format!(
            "expiry must be in DDMMMYY format such as 28OCT25, got {}",
            pyfmt::quote(&expiry_date)
        ));
    }
    let expiry_type = f.optional_text("expiryType")?.to_lowercase();
    if !expiry_type.is_empty() && !VALID_EXPIRY_TYPES.contains(&expiry_type.as_str()) {
        return Err(expiry_type_error(&expiry_type));
    }
    let option_type = f.choice("optionType", &OPTION_TYPES, None, &[])?;
    let action = f.choice("action", &VALID_ACTIONS, None, &[])?;
    let quantity_lots = f.integer("quantity", None, &[], Some(1))?;
    let product = f.choice(
        "product",
        &VALID_PRODUCT_TYPES,
        Some(d.product.as_str()),
        &[],
    )?;
    let pricetype = f.choice(
        "priceType",
        &VALID_PRICE_TYPES,
        Some(d.price_type.as_str()),
        &["pricetype"],
    )?;
    let price = f.number("price", Some(d.price), &[])?;
    let trigger_price = f.number("triggerPrice", Some(d.trigger_price), &["trigger_price"])?;
    let splitsize = f.integer("splitSize", Some(0), &["splitsize"], Some(0))?;
    if let Some(reason) = invalid_price_reason(&pricetype, price, trigger_price) {
        return Err(reason);
    }
    Ok(CustomLeg {
        strike,
        expiry_date,
        expiry_type,
        option_type,
        action,
        quantity_lots,
        pricetype,
        product,
        price,
        trigger_price,
        splitsize,
    })
}

/// `expiryType must be one of ...` for an unknown relative expiry.
pub fn expiry_type_error(expiry_type: &str) -> String {
    format!(
        "expiryType must be one of {}, got {}",
        VALID_EXPIRY_TYPES.join(", "),
        pyfmt::quote(expiry_type)
    )
}

/// What a readymade strategy's legs share.
pub struct Generated<'a> {
    pub action: &'a str,
    /// Contracts per leg (lots times the lot size).
    pub quantity: i64,
    pub product: &'a str,
    pub strangle_width: &'a str,
    pub price_type: &'a str,
    pub price: f64,
}

/// `_generate_strategy_legs`: the legs of a readymade strategy. Each leg
/// carries the node's price type and price but no trigger price (the panel
/// offers MARKET and LIMIT only). An unknown strategy has no legs.
pub fn generate_strategy_legs(ctx: &NodeCtx, strategy_type: &str, g: &Generated<'_>) -> Vec<Value> {
    let leg = |offset: &str, option_type: &str, action: &str| {
        json!({
            "offset": offset, "option_type": option_type, "action": action,
            "quantity": g.quantity, "pricetype": g.price_type, "product": g.product,
            "price": g.price, "splitsize": 0,
        })
    };
    let a = g.action;
    match strategy_type {
        "straddle" => vec![leg("ATM", "CE", a), leg("ATM", "PE", a)],
        "strangle" => vec![
            leg(g.strangle_width, "CE", a),
            leg(g.strangle_width, "PE", a),
        ],
        "iron_condor" => vec![
            leg("OTM2", "CE", "SELL"),
            leg("OTM4", "CE", "BUY"),
            leg("OTM2", "PE", "SELL"),
            leg("OTM4", "PE", "BUY"),
        ],
        "bull_call_spread" => vec![leg("ATM", "CE", "BUY"), leg("OTM2", "CE", "SELL")],
        "bear_put_spread" => vec![leg("ATM", "PE", "BUY"), leg("OTM2", "PE", "SELL")],
        "custom" => {
            ctx.log("Custom strategy selected but no legs provided", WARNING);
            Vec::new()
        }
        other => {
            ctx.log(format!("Unknown strategy type: {}", other), WARNING);
            Vec::new()
        }
    }
}

/// Python's `f"{x:g}"`: six significant digits, trailing zeros dropped,
/// scientific notation outside 1e-4 to 1e6 (`24500.0` is `24500`).
pub fn py_g(x: f64) -> String {
    if !x.is_finite() {
        return pyfmt::float(x);
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let sci = format!("{:.5e}", x);
    let (mantissa, exp) = match sci.split_once('e') {
        Some((m, e)) => (m.to_string(), e.parse::<i32>().unwrap_or(0)),
        None => (sci.clone(), 0),
    };
    if (-4..6).contains(&exp) {
        let decimals = usize::try_from(5 - exp).unwrap_or(0);
        trim_zeros(&format!("{:.*}", decimals, x))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{}{:02}", trim_zeros(&mantissa), sign, exp.abs())
    }
}

fn trim_zeros(text: &str) -> String {
    if !text.contains('.') {
        return text.to_string();
    }
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// One leg's line in the run log: `  Leg {n}: {strike or offset}[ {expiry}]
/// {type} {action} qty={quantity}`.
pub fn leg_line(n: usize, leg: &Value) -> String {
    let selector = match leg.get("strike").filter(|s| !s.is_null()) {
        Some(s) => py_g(s.as_f64().unwrap_or(f64::NAN)),
        None => leg
            .get("offset")
            .map(pyfmt::str)
            .unwrap_or_else(|| "None".into()),
    };
    let expiry = leg
        .get("expiry_date")
        .filter(|e| pyfmt::truthy(e))
        .map(|e| format!(" {}", pyfmt::str(e)))
        .unwrap_or_default();
    let text = |k: &str| leg.get(k).map(pyfmt::str).unwrap_or_default();
    format!(
        "  Leg {}: {}{} {} {} qty={}",
        n,
        selector,
        expiry,
        text("option_type"),
        text("action"),
        text("quantity")
    )
}

#[cfg(test)]
mod tests;
