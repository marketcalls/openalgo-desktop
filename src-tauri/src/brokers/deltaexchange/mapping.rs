//! OpenAlgo <-> Delta Exchange shapes (web `mapping/transform_data.py`,
//! `mapping/order_data.py`).
//!
//! * Order ids are composite `"{product_id}:{id}"`: Delta needs the product
//!   to cancel or modify, and the composite avoids a lookup.
//! * Sizes are contracts for derivatives (whole numbers) and units for spot
//!   (fractional allowed).
//! * Product types do not exist on Delta: books report `NRML` (spot wallet
//!   rows `CNC`).
//! * Prices are decimal strings; `limit_price` / `stop_price` are sent as
//!   strings like the web (`str(float)`).

use crate::brokers::common::de::string_lenient;
use crate::brokers::common::mapping::{PriceType, Validity};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, NaiveDateTime};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::str::FromStr;

/// The OpenAlgo exchange of every Delta instrument.
pub const EXCHANGE: &str = "CRYPTO";

/// Python `str(float)` for the prices the web sends (`60000.0`, `0.5`).
pub fn py_str(x: f64) -> String {
    if !x.is_finite() {
        return "0".into();
    }
    format!("{:?}", x)
}

/// Lenient decimal from Delta's number-or-string fields.
pub fn dec(s: &str) -> Decimal {
    let t = s.trim();
    Decimal::from_str(t)
        .or_else(|_| Decimal::from_scientific(t))
        .unwrap_or(Decimal::ZERO)
}

fn f(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

/// OpenAlgo price type -> Delta `order_type` (web `map_order_type`).
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market | PriceType::SlM => "market_order",
        PriceType::Limit | PriceType::Sl => "limit_order",
    }
}

/// Delta order state -> OpenAlgo status (web `map_order_data` plus the
/// `rejected` mapping of `calculate_order_statistics`).
pub fn map_status(state: &str) -> String {
    match state.trim().to_ascii_lowercase().as_str() {
        "open" | "pending" => "open".into(),
        "closed" | "filled" => "complete".into(),
        "cancelled" => "cancelled".into(),
        "rejected" => "rejected".into(),
        other => other.to_string(),
    }
}

/// Delta `order_type` + `stop_order_type` -> OpenAlgo price type.
pub fn reverse_order_type(order_type: &str, stop_order_type: &str) -> String {
    if stop_order_type == "stop_loss_order" {
        return if order_type == "limit_order" {
            "SL"
        } else {
            "SL-M"
        }
        .into();
    }
    match order_type {
        "limit_order" => "LIMIT".into(),
        "market_order" => "MARKET".into(),
        other => other.to_ascii_uppercase(),
    }
}

/// Split a composite `"{product_id}:{id}"`. A bare id (legacy) has no
/// product. Ids must be numeric.
pub fn parse_order_id(order_id: &str) -> Result<(Option<i64>, i64)> {
    let bad = || {
        AppError::Validation(format!(
            "Order id {} is not a Delta Exchange order id.",
            order_id
        ))
    };
    let num = |s: &str| s.trim().parse::<i64>().map_err(|_| bad());
    match order_id.split_once(':') {
        Some((p, id)) => Ok((Some(num(p)?), num(id)?)),
        None => Ok((None, num(order_id)?)),
    }
}

fn product_id(inst: &SymToken) -> Result<i64> {
    inst.token.trim().parse::<i64>().map_err(|_| {
        AppError::Validation(format!(
            "{} has no Delta Exchange product id. Download the master contract again from the broker page.",
            inst.symbol
        ))
    })
}

/// The `size` Delta expects (web `_order_size`): spot keeps a fractional
/// size, derivatives must be whole contracts.
pub fn order_size(q: &CryptoQuantity, inst: &SymToken) -> Result<Value> {
    if inst.instrument_type == "SPOT" {
        let v = q.as_decimal().to_f64().unwrap_or(0.0);
        return serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| AppError::Validation("Quantity must be a positive number.".into()));
    }
    match q.as_whole() {
        Some(n) => Ok(json!(n)),
        None => Err(AppError::Validation(format!(
            "Fractional quantity ({}) not allowed for derivative contracts. Use whole numbers for {}.",
            q, inst.symbol
        ))),
    }
}

/// `POST /v2/orders` body (web `transform_data`).
pub fn place_payload(o: &ResolvedOrder, q: &CryptoQuantity) -> Result<Value> {
    let ot = order_type(o.pricetype);
    let mut m = Map::new();
    m.insert("product_id".into(), json!(product_id(&o.instrument)?));
    m.insert("product_symbol".into(), json!(o.brsymbol()));
    m.insert("size".into(), order_size(q, &o.instrument)?);
    m.insert("side".into(), json!(o.action.as_str().to_ascii_lowercase()));
    m.insert("order_type".into(), json!(ot));
    m.insert(
        "time_in_force".into(),
        json!(if o.validity == Validity::Ioc {
            "ioc"
        } else {
            "gtc"
        }),
    );
    if ot == "limit_order" {
        m.insert(
            "limit_price".into(),
            json!(if o.price != 0.0 {
                py_str(o.price)
            } else {
                "0".into()
            }),
        );
    }
    if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) {
        m.insert("stop_order_type".into(), json!("stop_loss_order"));
        m.insert(
            "stop_price".into(),
            json!(if o.trigger_price != 0.0 {
                py_str(o.trigger_price)
            } else {
                "0".into()
            }),
        );
        m.insert("stop_trigger_method".into(), json!("last_traded_price"));
    }
    Ok(Value::Object(m))
}

/// `PUT /v2/orders` body (web `transform_modify_order_data`).
pub fn modify_payload(m: &ResolvedModify, q: &CryptoQuantity) -> Result<Value> {
    let (pid, id) = parse_order_id(&m.order_id)?;
    let pid = match pid {
        Some(p) => p,
        None => product_id(&m.instrument)?,
    };
    let limit_price = if m.pricetype == PriceType::SlM {
        "0".to_string()
    } else {
        py_str(m.price)
    };
    let mut body = Map::new();
    body.insert("id".into(), json!(id));
    body.insert("product_id".into(), json!(pid));
    body.insert("size".into(), order_size(q, &m.instrument)?);
    body.insert("limit_price".into(), json!(limit_price));
    if matches!(m.pricetype, PriceType::Sl | PriceType::SlM) {
        body.insert(
            "stop_price".into(),
            json!(if m.trigger_price != 0.0 {
                py_str(m.trigger_price)
            } else {
                "0".into()
            }),
        );
    }
    Ok(Value::Object(body))
}

/// `DELETE /v2/orders` body (web `cancel_order`).
pub fn cancel_body(order_id: &str) -> Result<Value> {
    Ok(match parse_order_id(order_id)? {
        (Some(p), id) => json!({"id": id, "product_id": p}),
        (None, id) => json!({"id": id}),
    })
}

/// Whether a UTC `created_at` (`2026-10-03T05:00:00.123456Z`) falls on the
/// IST calendar day `today` (web: the order and trade books show today in
/// IST although the venue runs 24x7 on UTC).
pub fn is_on_ist_day(created_at: &str, today: NaiveDate) -> bool {
    let head: String = created_at.chars().take(19).collect();
    NaiveDateTime::parse_from_str(&head, "%Y-%m-%dT%H:%M:%S")
        .map(|utc| (utc + chrono::Duration::minutes(330)).date() == today)
        .unwrap_or(false)
}

/// IST calendar date of an epoch-seconds instant.
pub fn ist_date(epoch_secs: i64) -> NaiveDate {
    chrono::DateTime::from_timestamp(epoch_secs + 19_800, 0)
        .map(|d| d.date_naive())
        .unwrap_or_default()
}

fn truthy_id(s: &str) -> bool {
    !s.is_empty() && s != "0" && s != "null"
}

fn oa_symbol_by_token(symbols: &SymbolResolver, product_id: &str, fallback: &str) -> String {
    if truthy_id(product_id) {
        if let Some(r) = symbols.by_token(EXCHANGE, product_id) {
            return r.symbol;
        }
    }
    fallback.to_string()
}

fn whole(d: Decimal) -> i32 {
    d.trunc().to_i32().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// One `/v2/orders` row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DeltaOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub side: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub stop_order_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub state: String,
    #[serde(deserialize_with = "string_lenient")]
    pub size: String,
    #[serde(deserialize_with = "string_lenient")]
    pub unfilled_size: String,
    #[serde(deserialize_with = "string_lenient")]
    pub limit_price: String,
    #[serde(deserialize_with = "string_lenient")]
    pub stop_price: String,
    #[serde(deserialize_with = "string_lenient")]
    pub average_fill_price: String,
    #[serde(deserialize_with = "string_lenient")]
    pub time_in_force: String,
    #[serde(deserialize_with = "string_lenient")]
    pub created_at: String,
}

impl DeltaOrder {
    pub fn composite_id(&self) -> String {
        if truthy_id(&self.product_id) {
            format!("{}:{}", self.product_id, self.id)
        } else {
            self.id.clone()
        }
    }
}

/// Order-book row (web `map_order_data` + `transform_order_data`).
pub fn map_order(o: &DeltaOrder, symbols: &SymbolResolver) -> Order {
    let size = dec(&o.size);
    let unfilled = if o.unfilled_size.is_empty() {
        size
    } else {
        dec(&o.unfilled_size)
    };
    Order {
        order_id: o.composite_id(),
        exchange_order_id: None,
        symbol: oa_symbol_by_token(symbols, &o.product_id, &o.product_symbol),
        exchange: EXCHANGE.into(),
        side: o.side.to_ascii_uppercase(),
        quantity: whole(size),
        filled_quantity: whole((size - unfilled).max(Decimal::ZERO)),
        pending_quantity: whole(unfilled),
        price: f(&o.limit_price),
        trigger_price: f(&o.stop_price),
        average_price: f(&o.average_fill_price),
        order_type: reverse_order_type(&o.order_type, &o.stop_order_type),
        product: "NRML".into(),
        status: map_status(&o.state),
        validity: if o.time_in_force.is_empty() {
            "GTC".into()
        } else {
            o.time_in_force.to_ascii_uppercase()
        },
        order_timestamp: o.created_at.clone(),
        exchange_timestamp: None,
        rejection_reason: None,
    }
}

/// One `/v2/fills` row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DeltaFill {
    #[serde(deserialize_with = "string_lenient")]
    pub id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub side: String,
    #[serde(deserialize_with = "string_lenient")]
    pub size: String,
    #[serde(deserialize_with = "string_lenient")]
    pub price: String,
    #[serde(deserialize_with = "string_lenient")]
    pub created_at: String,
}

/// Trade-book row (web `map_trade_data` + `transform_tradebook_data`).
pub fn map_trade(t: &DeltaFill, symbols: &SymbolResolver) -> Trade {
    let size = f(&t.size);
    let price = f(&t.price);
    Trade {
        order_id: if truthy_id(&t.product_id) {
            format!("{}:{}", t.product_id, t.order_id)
        } else {
            t.order_id.clone()
        },
        trade_id: t.id.clone(),
        symbol: oa_symbol_by_token(symbols, &t.product_id, &t.product_symbol),
        exchange: EXCHANGE.into(),
        product: "NRML".into(),
        side: t.side.to_ascii_uppercase(),
        quantity: whole(dec(&t.size)),
        average_price: price,
        trade_value: size * price,
        timestamp: t.created_at.clone(),
    }
}

/// One `/v2/positions/margined` row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DeltaPosition {
    #[serde(deserialize_with = "string_lenient")]
    pub product_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub size: String,
    #[serde(deserialize_with = "string_lenient")]
    pub entry_price: String,
    #[serde(deserialize_with = "string_lenient")]
    pub realized_pnl: String,
    #[serde(deserialize_with = "string_lenient")]
    pub unrealized_pnl: String,
}

/// One `/v2/wallet/balances` row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WalletBalance {
    #[serde(deserialize_with = "string_lenient")]
    pub asset_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub asset_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub balance: String,
    #[serde(deserialize_with = "string_lenient")]
    pub blocked_margin: String,
    #[serde(deserialize_with = "string_lenient")]
    pub balance_inr: String,
    #[serde(deserialize_with = "string_lenient")]
    pub cross_locked_collateral: String,
}

/// A derivative position or a spot wallet balance, with its exact size.
#[derive(Debug, Clone, PartialEq)]
pub struct RawPosition {
    pub product_id: String,
    pub product_symbol: String,
    pub size: Decimal,
    pub entry_price: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    /// Synthesised from a wallet balance (`{ASSET}_INR`).
    pub is_spot: bool,
}

impl From<&DeltaPosition> for RawPosition {
    fn from(p: &DeltaPosition) -> Self {
        Self {
            product_id: p.product_id.clone(),
            product_symbol: p.product_symbol.clone(),
            size: dec(&p.size),
            entry_price: f(&p.entry_price),
            realized_pnl: f(&p.realized_pnl),
            unrealized_pnl: f(&p.unrealized_pnl),
            is_spot: false,
        }
    }
}

/// Spot holdings as positions (web `get_positions` step 2): every asset
/// other than INR/USD with `balance - blocked_margin > 0` becomes
/// `{ASSET}_INR`.
pub fn spot_positions(balances: &[WalletBalance]) -> Vec<RawPosition> {
    balances
        .iter()
        .filter_map(|a| {
            let sym = if a.asset_symbol.is_empty() {
                &a.symbol
            } else {
                &a.asset_symbol
            };
            if sym.is_empty() || sym == "INR" || sym == "USD" {
                return None;
            }
            let size = dec(&a.balance) - dec(&a.blocked_margin);
            (size > Decimal::ZERO).then(|| RawPosition {
                product_id: a.asset_id.clone(),
                product_symbol: format!("{}_INR", sym),
                size,
                entry_price: 0.0,
                realized_pnl: 0.0,
                unrealized_pnl: 0.0,
                is_spot: true,
            })
        })
        .collect()
}

/// The OpenAlgo master row of a position: spot by broker symbol (the
/// wallet's `asset_id` is not a product id), derivatives by product id.
pub fn position_row(p: &RawPosition, symbols: &SymbolResolver) -> Option<SymToken> {
    if p.is_spot {
        symbols.by_brsymbol(EXCHANGE, &p.product_symbol)
    } else if truthy_id(&p.product_id) {
        symbols.by_token(EXCHANGE, &p.product_id)
    } else {
        None
    }
}

/// Position-book row (web `map_position_data` + `transform_positions_data`).
/// `None` for a spot balance that is not a whole number of units: the
/// shared position row carries whole units, and a truncated size would
/// misstate the holding (it stays visible to close-all, which works on the
/// exact size).
pub fn map_position(p: &RawPosition, symbols: &SymbolResolver) -> Option<Position> {
    if !p.size.fract().is_zero() {
        tracing::debug!(
            "Delta Exchange position {} has a fractional size; not shown in the position book",
            p.product_symbol
        );
        return None;
    }
    let symbol = position_row(p, symbols)
        .map(|r| r.symbol)
        .unwrap_or_else(|| p.product_symbol.clone());
    Some(Position {
        symbol,
        exchange: EXCHANGE.into(),
        product: if p.is_spot { "CNC" } else { "NRML" }.into(),
        quantity: whole(p.size),
        overnight_quantity: 0,
        average_price: p.entry_price,
        ltp: 0.0,
        pnl: p.realized_pnl + p.unrealized_pnl,
        realized_pnl: p.realized_pnl,
        unrealized_pnl: p.unrealized_pnl,
        buy_quantity: 0,
        buy_value: 0.0,
        sell_quantity: 0,
        sell_value: 0.0,
    })
}
