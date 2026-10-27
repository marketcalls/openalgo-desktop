//! An order as the guard checks it (the web's `_Intent` and `_normalise`).
//!
//! The caller's arguments arrive as JSON values, exactly what the web's
//! guard accepts as `Any`; every check works on the normalised [`Intent`],
//! never on the caller's text. A value that cannot be normalised is a
//! refusal with its code, worded as the web words it.

use super::num::{positive, py_decimal, py_repr, py_str, py_text_or_empty};
use super::RiskCode;
use rust_decimal::Decimal;
use serde_json::{Map, Value};

pub const MARKET: &str = "MARKET";
pub const LIMIT: &str = "LIMIT";
pub const SL: &str = "SL";
pub const SL_M: &str = "SL-M";
pub const VALID_PRICE_TYPES: [&str; 4] = [MARKET, LIMIT, SL, SL_M];
pub const VALID_ACTIONS: [&str; 2] = ["BUY", "SELL"];

/// The keys an order (or a basket leg) may carry.
pub const ORDER_KEYS: [&str; 10] = [
    "symbol",
    "exchange",
    "action",
    "quantity",
    "product",
    "price_type",
    "price",
    "trigger_price",
    "ltp",
    "available_funds",
];

/// The keys `_normalise` cannot do without, in its signature's order.
pub const REQUIRED_KEYS: [&str; 5] = ["symbol", "exchange", "action", "quantity", "product"];

/// A normalised, checkable order.
#[derive(Debug, Clone, PartialEq)]
pub struct Intent {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub product: String,
    pub price_type: String,
    pub price: Option<Decimal>,
    pub trigger_price: Option<Decimal>,
    pub ltp: Option<Decimal>,
    pub available_funds: Option<Decimal>,
}

impl Intent {
    /// The identity the duplicate-order window compares: the canonical
    /// symbol, exchange, side, whole quantity, product and price type, and
    /// only the prices that type uses, each without its scale.
    ///
    /// Desktop fix (security review, 2026-10-27): the web joins the prices
    /// as written, so the same order restated as `100.0` instead of `100`,
    /// or a MARKET order carrying a different (ignored) price, read as a
    /// new order and escaped the window.
    pub fn fingerprint(&self) -> String {
        let text = |d: Option<Decimal>| d.map(|x| x.normalize().to_string()).unwrap_or_default();
        let (price, trigger) = match self.price_type.as_str() {
            MARKET => (None, None),
            LIMIT => (self.price, None),
            SL_M => (None, self.trigger_price),
            _ => (self.price, self.trigger_price),
        };
        [
            self.symbol.clone(),
            self.exchange.clone(),
            self.action.clone(),
            self.quantity.to_string(),
            self.product.clone(),
            self.price_type.clone(),
            text(price),
            text(trigger),
        ]
        .join("|")
    }

    /// What the order is worth per unit: the limit price of a priced order,
    /// else the last traded price, else a stop-market order's trigger.
    pub fn reference_price(&self) -> Option<Decimal> {
        if self.price_type == LIMIT || self.price_type == SL {
            if let Some(p) = self.price {
                return Some(p);
            }
        }
        if self.ltp.is_some() {
            return self.ltp;
        }
        if self.price_type == SL_M {
            return self.trigger_price;
        }
        None
    }

    /// The price the deviation check compares with the last traded price.
    pub fn deviation_price(&self) -> Option<Decimal> {
        match self.price_type.as_str() {
            LIMIT | SL => self.price,
            SL_M => self.trigger_price,
            _ => None,
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {} {} on {}",
            self.action, self.quantity, self.symbol, self.exchange
        )
    }
}

/// An order that cannot be normalised into something checkable.
#[derive(Debug, Clone, PartialEq)]
pub struct Invalid {
    pub code: RiskCode,
    pub reason: String,
    pub details: Map<String, Value>,
}

fn invalid(code: RiskCode, reason: String, details: &[(&str, Value)]) -> Invalid {
    Invalid {
        code,
        reason,
        details: details
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    }
}

static NULL: Value = Value::Null;

/// One argument; an absent one reads as `None`, the web's default.
pub fn arg<'a>(args: &'a Map<String, Value>, key: &str) -> &'a Value {
    args.get(key).unwrap_or(&NULL)
}

/// The web's `_normalise`, check for check and sentence for sentence.
pub fn normalise(args: &Map<String, Value>) -> Result<Intent, Invalid> {
    let symbol = py_text_or_empty(arg(args, "symbol")).trim().to_uppercase();
    if symbol.is_empty() {
        return Err(invalid(
            RiskCode::InvalidSymbol,
            "No symbol was supplied.".into(),
            &[],
        ));
    }
    let exchange = py_text_or_empty(arg(args, "exchange"))
        .trim()
        .to_uppercase();
    if exchange.is_empty() {
        return Err(invalid(
            RiskCode::ExchangeNotAllowed,
            format!("No exchange was supplied for {}.", symbol),
            &[],
        ));
    }
    let raw_action = arg(args, "action");
    let action = py_text_or_empty(raw_action).trim().to_uppercase();
    if !VALID_ACTIONS.contains(&action.as_str()) {
        return Err(invalid(
            RiskCode::InvalidAction,
            format!("Action must be BUY or SELL, not {}.", py_repr(raw_action)),
            &[("action", Value::String(py_str(raw_action)))],
        ));
    }
    let product = py_text_or_empty(arg(args, "product")).trim().to_uppercase();
    if product.is_empty() {
        return Err(invalid(
            RiskCode::ProductNotAllowed,
            format!("No product type was supplied for {}.", symbol),
            &[],
        ));
    }
    let price_type = price_type(arg(args, "price_type"))?;
    let quantity = quantity(arg(args, "quantity"))?;
    let price = positive(py_decimal(arg(args, "price")));
    let trigger_price = positive(py_decimal(arg(args, "trigger_price")));
    if (price_type == LIMIT || price_type == SL) && price.is_none() {
        return Err(invalid(
            RiskCode::InvalidPrice,
            format!("A {} order needs a positive limit price.", price_type),
            &[("price", Value::String(py_str(arg(args, "price"))))],
        ));
    }
    if (price_type == SL || price_type == SL_M) && trigger_price.is_none() {
        return Err(invalid(
            RiskCode::InvalidPrice,
            format!("A {} order needs a positive trigger price.", price_type),
            &[(
                "trigger_price",
                Value::String(py_str(arg(args, "trigger_price"))),
            )],
        ));
    }
    Ok(Intent {
        symbol,
        exchange,
        action,
        quantity,
        product,
        price_type,
        price,
        trigger_price,
        ltp: positive(py_decimal(arg(args, "ltp"))),
        available_funds: py_decimal(arg(args, "available_funds")),
    })
}

fn price_type(raw: &Value) -> Result<String, Invalid> {
    let text = if super::num::py_truthy(raw) {
        py_str(raw)
    } else {
        MARKET.to_string()
    };
    let mut kind = text.trim().to_uppercase();
    if kind == "SLM" {
        kind = SL_M.to_string();
    }
    if VALID_PRICE_TYPES.contains(&kind.as_str()) {
        Ok(kind)
    } else {
        Err(invalid(
            RiskCode::InvalidPriceType,
            format!(
                "Price type must be one of {}, not {}.",
                VALID_PRICE_TYPES.join(", "),
                py_repr(raw)
            ),
            &[("price_type", Value::String(py_str(raw)))],
        ))
    }
}

/// `int(str(quantity).strip())`: a float such as `10.0` is not a whole
/// number to the web's guard, and neither is `True`.
fn quantity(raw: &Value) -> Result<i64, Invalid> {
    let Ok(value) = py_str(raw).trim().parse::<i64>() else {
        return Err(invalid(
            RiskCode::InvalidQuantity,
            format!("Quantity must be a whole number, not {}.", py_repr(raw)),
            &[("quantity", Value::String(py_str(raw)))],
        ));
    };
    if value <= 0 {
        return Err(invalid(
            RiskCode::InvalidQuantity,
            format!("Quantity must be greater than zero, not {}.", value),
            &[("quantity", Value::from(value))],
        ));
    }
    Ok(value)
}

/// What Python says when a basket leg lacks keys `_normalise` requires
/// (its `TypeError`), so the leg's refusal reads as the web's.
pub fn missing_keys_message(missing: &[&str]) -> String {
    let quoted: Vec<String> = missing.iter().map(|k| format!("'{}'", k)).collect();
    let list = match quoted.len() {
        0 => String::new(),
        1 => quoted[0].clone(),
        2 => format!("{} and {}", quoted[0], quoted[1]),
        n => format!("{}, and {}", quoted[..n - 1].join(", "), quoted[n - 1]),
    };
    format!(
        "_normalise() missing {} required keyword-only argument{}: {}",
        missing.len(),
        if missing.len() == 1 { "" } else { "s" },
        list
    )
}
