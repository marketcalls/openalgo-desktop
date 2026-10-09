//! Dhan <-> OpenAlgo translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`).
//!
//! Every book normaliser resolves Dhan's `securityId` back to the OpenAlgo
//! symbol through the master contract (token-keyed, like the web's
//! `get_symbol(token, exchange)`), never trusting Dhan's `tradingSymbol`.

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::mapping::{Action, PriceType, Product, Validity};
use crate::brokers::common::mpp::{instrument_type_from_symbol, mpp_percentage, py_round};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Enum maps
// ---------------------------------------------------------------------------

/// OpenAlgo exchange -> Dhan `exchangeSegment` for orders, margin and GTT
/// (web `map_exchange_type`; no index segments).
pub fn exchange_segment(exchange: &str) -> Option<&'static str> {
    Some(match exchange {
        "NSE" => "NSE_EQ",
        "BSE" => "BSE_EQ",
        "CDS" => "NSE_CURRENCY",
        "NFO" => "NSE_FNO",
        "BFO" => "BSE_FNO",
        "BCD" => "BSE_CURRENCY",
        "MCX" => "MCX_COMM",
        "NCO" => "NSE_COMM",
        _ => return None,
    })
}

/// OpenAlgo exchange -> Dhan segment for quotes, history and the feed
/// (web `_get_exchange_segment`, adds `IDX_I`).
pub fn data_segment(exchange: &str) -> Option<&'static str> {
    match exchange {
        "NSE_INDEX" | "BSE_INDEX" => Some("IDX_I"),
        other => exchange_segment(other),
    }
}

/// Dhan segment -> OpenAlgo exchange (web `map_exchange`; unknown segments
/// pass through so a row is never blank).
pub fn map_exchange(segment: &str) -> String {
    match segment {
        "NSE_EQ" => "NSE",
        "BSE_EQ" => "BSE",
        "NSE_CURRENCY" => "CDS",
        "NSE_FNO" => "NFO",
        "BSE_FNO" => "BFO",
        "BSE_CURRENCY" => "BCD",
        "MCX_COMM" => "MCX",
        "NSE_COMM" => "NCO",
        other => other,
    }
    .to_string()
}

/// OpenAlgo product -> Dhan `productType`.
pub fn product_type(p: Product) -> &'static str {
    match p {
        Product::Cnc => "CNC",
        Product::Nrml => "MARGIN",
        Product::Mis => "INTRADAY",
    }
}

/// Dhan `productType` -> OpenAlgo product (web `reverse_map_product_type`).
pub fn reverse_product(p: &str) -> Option<&'static str> {
    match p {
        "CNC" => Some("CNC"),
        "MARGIN" => Some("NRML"),
        "INTRADAY" => Some("MIS"),
        _ => None,
    }
}

/// Book rows (web `map_order_data`): INTRADAY is MIS everywhere, MARGIN is
/// NRML on the derivative exchanges; anything else passes through.
pub fn book_product(product: &str, exchange: &str) -> String {
    match product {
        "INTRADAY" => "MIS".into(),
        "MARGIN" if matches!(exchange, "NFO" | "MCX" | "BFO" | "CDS" | "BCD" | "NCO") => {
            "NRML".into()
        }
        other => other.to_string(),
    }
}

/// OpenAlgo price type -> Dhan `orderType` (before the SL-M conversion).
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "STOP_LOSS",
        PriceType::SlM => "STOP_LOSS_MARKET",
    }
}

/// Dhan `orderType` -> OpenAlgo price type.
pub fn reverse_order_type(t: &str) -> String {
    match t {
        "STOP_LOSS" => "SL".into(),
        "STOP_LOSS_MARKET" => "SL-M".into(),
        other => other.to_string(),
    }
}

/// Dhan REST `orderStatus` -> OpenAlgo status (web
/// `calculate_order_statistics`); other states are lowercased.
pub fn map_status(s: &str) -> String {
    match s.trim() {
        "TRADED" => "complete".into(),
        "PENDING" => "open".into(),
        "REJECTED" => "rejected".into(),
        "CANCELLED" => "cancelled".into(),
        other => crate::brokers::lower_status(other),
    }
}

// ---------------------------------------------------------------------------
// SL-M protective limit (web `_slm_protected_price`)
// ---------------------------------------------------------------------------

/// Decimal places implied by a tick (`0.05` -> 2, `0.0025` -> 4).
pub fn tick_decimals(tick: f64) -> i32 {
    let s = format!("{}", tick);
    match s.split_once('.') {
        Some((_, frac)) => frac.len() as i32,
        None => 0,
    }
}

/// Snap to a multiple of `tick`, flooring or ceiling (web `_snap_to_tick`).
pub fn snap_to_tick(value: f64, tick: f64, floor: bool) -> f64 {
    let ratio = py_round(value / tick, 6);
    let k = if floor { ratio.floor() } else { ratio.ceil() };
    py_round(k * tick, tick_decimals(tick))
}

/// The protective STOP_LOSS limit Dhan needs in place of an SL-M: one MPP
/// band past the trigger in the fill direction, at least one tick away,
/// tick-aligned away from the trigger. Fails closed without a tick size.
pub fn slm_protected_price(symbol: &str, action: Action, trigger: f64, tick: f64) -> Result<f64> {
    if !tick.is_finite() || tick <= 0.0 {
        return Err(AppError::Validation(format!(
            "Cannot place the SL-M order for {}: the master contract has no tick size for it. Download the master contract again, then retry.",
            symbol
        )));
    }
    let pct = mpp_percentage(trigger, instrument_type_from_symbol(symbol)) / 100.0;
    match action {
        Action::Sell => {
            let raw = (trigger * (1.0 - pct)).min(trigger - tick);
            let limit = snap_to_tick(raw, tick, true);
            if limit <= 0.0 {
                return Err(AppError::Validation(format!(
                    "The SL-M trigger {} for {} is too low to place a protected stop. Use an SL order with your own limit price.",
                    trigger, symbol
                )));
            }
            Ok(limit)
        }
        Action::Buy => {
            let raw = (trigger * (1.0 + pct)).max(trigger + tick);
            Ok(snap_to_tick(raw, tick, false))
        }
    }
}

fn trigger_required() -> AppError {
    AppError::Validation("Trigger price is required for Stop Loss orders".into())
}

/// `POST /v2/orders` body (web `transform_data`). `convert_slm` is false on
/// the sandbox, whose mapper sends a bare STOP_LOSS_MARKET.
pub fn place_order_body(o: &ResolvedOrder, client_id: &str, convert_slm: bool) -> Result<Value> {
    let segment = exchange_segment(o.exchange.as_str()).ok_or_else(|| {
        AppError::Validation(format!("Dhan does not accept orders on {}.", o.exchange))
    })?;
    let mut m = Map::new();
    m.insert("dhanClientId".into(), json!(client_id));
    m.insert("transactionType".into(), json!(o.action.as_str()));
    m.insert("exchangeSegment".into(), json!(segment));
    m.insert("productType".into(), json!(product_type(o.product)));
    m.insert("orderType".into(), json!(order_type(o.pricetype)));
    m.insert("validity".into(), json!("DAY"));
    m.insert("securityId".into(), json!(o.token()));
    m.insert("quantity".into(), json!(o.quantity));
    if matches!(o.pricetype, PriceType::Limit | PriceType::Sl) {
        m.insert("price".into(), json!(o.price));
    }
    if o.disclosed_quantity > 0 {
        m.insert("disclosedQuantity".into(), json!(o.disclosed_quantity));
    }
    if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) {
        if o.trigger_price <= 0.0 {
            return Err(trigger_required());
        }
        m.insert("triggerPrice".into(), json!(o.trigger_price));
        if o.pricetype == PriceType::SlM && convert_slm {
            let limit =
                slm_protected_price(&o.symbol, o.action, o.trigger_price, o.instrument.tick_size)?;
            m.insert("orderType".into(), json!("STOP_LOSS"));
            m.insert("price".into(), json!(limit));
        }
    }
    if o.amo {
        m.insert("afterMarketOrder".into(), json!(true));
    }
    if o.validity == Validity::Ioc {
        m.insert("validity".into(), json!("IOC"));
    }
    Ok(Value::Object(m))
}

/// `PUT /v2/orders/{orderId}` body (web `transform_modify_order_data`). The
/// web sends the raw `BROKER_API_KEY` as `dhanClientId` here; the real
/// client id is sent instead.
pub fn modify_order_body(m: &ResolvedModify, client_id: &str, convert_slm: bool) -> Result<Value> {
    let mut b = Map::new();
    b.insert("dhanClientId".into(), json!(client_id));
    b.insert("orderId".into(), json!(m.order_id));
    b.insert("orderType".into(), json!(order_type(m.pricetype)));
    b.insert("legName".into(), json!("ENTRY_LEG"));
    b.insert("quantity".into(), json!(m.quantity));
    b.insert("validity".into(), json!("DAY"));
    if matches!(m.pricetype, PriceType::Limit | PriceType::Sl) {
        b.insert("price".into(), json!(m.price));
    }
    if m.disclosed_quantity > 0 {
        b.insert("disclosedQuantity".into(), json!(m.disclosed_quantity));
    }
    if matches!(m.pricetype, PriceType::Sl | PriceType::SlM) {
        if m.trigger_price <= 0.0 {
            return Err(trigger_required());
        }
        b.insert("triggerPrice".into(), json!(m.trigger_price));
        if m.pricetype == PriceType::SlM && convert_slm {
            let limit =
                slm_protected_price(&m.symbol, m.action, m.trigger_price, m.instrument.tick_size)?;
            b.insert("orderType".into(), json!("STOP_LOSS"));
            b.insert("price".into(), json!(limit));
        }
    }
    Ok(Value::Object(b))
}

// ---------------------------------------------------------------------------
// Dhan payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DhanOrder {
    #[serde(rename = "orderId", deserialize_with = "string_lenient")]
    pub order_id: String,
    #[serde(rename = "exchangeOrderId", deserialize_with = "string_lenient")]
    pub exchange_order_id: String,
    #[serde(rename = "orderStatus", deserialize_with = "string_lenient")]
    pub order_status: String,
    #[serde(rename = "transactionType", deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(rename = "exchangeSegment", deserialize_with = "string_lenient")]
    pub exchange_segment: String,
    #[serde(rename = "productType", deserialize_with = "string_lenient")]
    pub product_type: String,
    #[serde(rename = "orderType", deserialize_with = "string_lenient")]
    pub order_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub validity: String,
    #[serde(rename = "tradingSymbol", deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(rename = "securityId", deserialize_with = "string_lenient")]
    pub security_id: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(rename = "filledQty", deserialize_with = "i64_lenient")]
    pub filled_qty: i64,
    #[serde(rename = "remainingQuantity", deserialize_with = "i64_lenient")]
    pub remaining_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(rename = "triggerPrice", deserialize_with = "f64_lenient")]
    pub trigger_price: f64,
    #[serde(rename = "averageTradedPrice", deserialize_with = "f64_lenient")]
    pub average_traded_price: f64,
    #[serde(rename = "createTime", deserialize_with = "string_lenient")]
    pub create_time: String,
    #[serde(rename = "updateTime", deserialize_with = "string_lenient")]
    pub update_time: String,
    #[serde(rename = "exchangeTime", deserialize_with = "string_lenient")]
    pub exchange_time: String,
    #[serde(rename = "omsErrorDescription", deserialize_with = "string_lenient")]
    pub oms_error_description: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DhanTrade {
    #[serde(rename = "orderId", deserialize_with = "string_lenient")]
    pub order_id: String,
    #[serde(rename = "exchangeTradeId", deserialize_with = "string_lenient")]
    pub exchange_trade_id: String,
    #[serde(rename = "transactionType", deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(rename = "exchangeSegment", deserialize_with = "string_lenient")]
    pub exchange_segment: String,
    #[serde(rename = "productType", deserialize_with = "string_lenient")]
    pub product_type: String,
    #[serde(rename = "tradingSymbol", deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(rename = "securityId", deserialize_with = "string_lenient")]
    pub security_id: String,
    #[serde(rename = "tradedQuantity", deserialize_with = "i64_lenient")]
    pub traded_quantity: i64,
    #[serde(rename = "tradedPrice", deserialize_with = "f64_lenient")]
    pub traded_price: f64,
    #[serde(rename = "updateTime", deserialize_with = "string_lenient")]
    pub update_time: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DhanPosition {
    #[serde(rename = "tradingSymbol", deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(rename = "securityId", deserialize_with = "string_lenient")]
    pub security_id: String,
    #[serde(rename = "exchangeSegment", deserialize_with = "string_lenient")]
    pub exchange_segment: String,
    #[serde(rename = "productType", deserialize_with = "string_lenient")]
    pub product_type: String,
    #[serde(rename = "netQty", deserialize_with = "i64_lenient")]
    pub net_qty: i64,
    #[serde(rename = "costPrice", deserialize_with = "f64_lenient")]
    pub cost_price: f64,
    #[serde(rename = "buyQty", deserialize_with = "i64_lenient")]
    pub buy_qty: i64,
    #[serde(rename = "sellQty", deserialize_with = "i64_lenient")]
    pub sell_qty: i64,
    #[serde(rename = "dayBuyValue", deserialize_with = "f64_lenient")]
    pub day_buy_value: f64,
    #[serde(rename = "daySellValue", deserialize_with = "f64_lenient")]
    pub day_sell_value: f64,
    #[serde(rename = "carryForwardBuyQty", deserialize_with = "i64_lenient")]
    pub cf_buy_qty: i64,
    #[serde(rename = "carryForwardSellQty", deserialize_with = "i64_lenient")]
    pub cf_sell_qty: i64,
    #[serde(rename = "realizedProfit", deserialize_with = "f64_lenient")]
    pub realized_profit: f64,
    #[serde(rename = "unrealizedProfit", deserialize_with = "f64_lenient")]
    pub unrealized_profit: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DhanHolding {
    #[serde(rename = "tradingSymbol", deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(rename = "securityId", deserialize_with = "string_lenient")]
    pub security_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
    #[serde(rename = "totalQty", deserialize_with = "i64_lenient")]
    pub total_qty: i64,
    #[serde(rename = "t1Qty", deserialize_with = "i64_lenient")]
    pub t1_qty: i64,
    #[serde(rename = "avgCostPrice", deserialize_with = "f64_lenient")]
    pub avg_cost_price: f64,
}

/// Rows of a book response: Dhan answers a bare list (or `{"data": [...]}`
/// in places); anything else is an error body.
pub fn rows<T: serde::de::DeserializeOwned>(v: Value) -> Result<Vec<T>> {
    let list = match v {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("data") {
            Some(Value::Array(a)) => a,
            _ => {
                return Err(super::dhan_error(&Value::Object(o)).unwrap_or_else(|| {
                    AppError::Broker("Dhan returned an unexpected book. Try again shortly.".into())
                }))
            }
        },
        Value::Null => Vec::new(),
        _ => {
            return Err(AppError::Broker(
                "Dhan returned an unexpected book. Try again shortly.".into(),
            ))
        }
    };
    Ok(list
        .into_iter()
        .filter_map(|r| match serde_json::from_value::<T>(r) {
            Ok(x) => Some(x),
            Err(e) => {
                tracing::warn!("Dhan book row skipped: {}", e);
                None
            }
        })
        .collect())
}

/// Holdings "no holdings" error shapes (web `map_portfolio_data`).
pub fn is_no_holdings(v: &Value) -> bool {
    let g = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
    g("errorCode") == "DHOLDING_ERROR"
        || g("internalErrorCode") == "DH-1111"
        || g("internalErrorMessage") == "No holdings available"
}

// ---------------------------------------------------------------------------
// Book normalisers
// ---------------------------------------------------------------------------

/// OpenAlgo symbol for a Dhan row: token lookup, then the raw trading symbol,
/// then the security id (web `map_order_data`).
pub fn resolve_symbol(
    symbols: &SymbolResolver,
    security_id: &str,
    exchange: &str,
    trading_symbol: &str,
) -> String {
    if let Some(r) = symbols.by_token(exchange, security_id.trim()) {
        return r.symbol;
    }
    if !trading_symbol.is_empty() {
        tracing::debug!("No OpenAlgo symbol for Dhan {}:{}", exchange, security_id);
        return trading_symbol.to_string();
    }
    security_id.to_string()
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// `map_order_data` + `transform_order_data`.
pub fn map_orders(rows: Vec<DhanOrder>, symbols: &SymbolResolver) -> Vec<Order> {
    rows.into_iter()
        .map(|o| {
            let exchange = map_exchange(&o.exchange_segment);
            let status = map_status(&o.order_status);
            Order {
                order_tag: None,
                order_id: o.order_id.clone(),
                exchange_order_id: non_empty(&o.exchange_order_id),
                symbol: resolve_symbol(symbols, &o.security_id, &exchange, &o.trading_symbol),
                side: o.transaction_type.clone(),
                quantity: clamp_i32(o.quantity),
                filled_quantity: clamp_i32(o.filled_qty),
                pending_quantity: clamp_i32(o.remaining_quantity),
                price: o.price,
                trigger_price: o.trigger_price,
                average_price: o.average_traded_price,
                order_type: reverse_order_type(&o.order_type),
                product: book_product(&o.product_type, &exchange),
                validity: o.validity.clone(),
                order_timestamp: o.update_time.clone(),
                exchange_timestamp: non_empty(&o.exchange_time),
                rejection_reason: if status == "rejected" {
                    non_empty(&o.oms_error_description)
                } else {
                    None
                },
                status,
                exchange,
            }
        })
        .collect()
}

/// `map_trade_data` + `transform_tradebook_data`.
pub fn map_trades(rows: Vec<DhanTrade>, symbols: &SymbolResolver) -> Vec<Trade> {
    rows.into_iter()
        .map(|t| {
            let exchange = map_exchange(&t.exchange_segment);
            Trade {
                order_tag: None,
                order_id: t.order_id.clone(),
                trade_id: t.exchange_trade_id.clone(),
                symbol: resolve_symbol(symbols, &t.security_id, &exchange, &t.trading_symbol),
                product: book_product(&t.product_type, &exchange),
                side: t.transaction_type.clone(),
                quantity: clamp_i32(t.traded_quantity),
                average_price: t.traded_price,
                trade_value: t.traded_quantity as f64 * t.traded_price,
                timestamp: t.update_time.clone(),
                exchange,
            }
        })
        .collect()
}

/// `exchange:symbol` key of an LTP map.
pub fn ltp_key(exchange: &str, symbol: &str) -> String {
    format!("{}:{}", exchange, symbol)
}

/// `map_position_data` + `transform_positions_data`. Dhan sends no LTP; the
/// caller passes the multiquote LTPs keyed by `ltp_key`.
pub fn map_positions(
    rows: Vec<DhanPosition>,
    symbols: &SymbolResolver,
    ltp: &HashMap<String, f64>,
) -> Vec<Position> {
    rows.into_iter()
        .map(|p| {
            let exchange = map_exchange(&p.exchange_segment);
            let symbol = resolve_symbol(symbols, &p.security_id, &exchange, &p.trading_symbol);
            let last = ltp
                .get(&ltp_key(&exchange, &symbol))
                .copied()
                .unwrap_or(0.0);
            Position {
                product: book_product(&p.product_type, &exchange),
                quantity: clamp_i32(p.net_qty),
                overnight_quantity: clamp_i32(p.cf_buy_qty - p.cf_sell_qty),
                average_price: p.cost_price,
                ltp: py_round(last, 2),
                pnl: py_round(p.realized_profit + p.unrealized_profit, 2),
                realized_pnl: p.realized_profit,
                unrealized_pnl: p.unrealized_profit,
                buy_quantity: clamp_i32(p.buy_qty),
                buy_value: p.day_buy_value,
                sell_quantity: clamp_i32(p.sell_qty),
                sell_value: p.day_sell_value,
                symbol,
                exchange,
            }
        })
        .collect()
}

/// Real exchange and OpenAlgo symbol of a holding: Dhan says `ALL`, so the
/// security id is probed on NSE, then BSE (web `map_portfolio_data`).
pub fn holding_listing(symbols: &SymbolResolver, h: &DhanHolding) -> (String, String) {
    let id = h.security_id.trim();
    if !id.is_empty() {
        for ex in ["NSE", "BSE"] {
            if let Some(r) = symbols.by_token(ex, id) {
                return (ex.to_string(), r.symbol);
            }
        }
    }
    ("NSE".to_string(), h.trading_symbol.clone())
}

/// `transform_holdings_data`; `ltp` keyed by `ltp_key`.
pub fn map_holdings(
    rows: Vec<DhanHolding>,
    symbols: &SymbolResolver,
    ltp: &HashMap<String, f64>,
) -> Vec<Holding> {
    rows.into_iter()
        .map(|h| {
            let (exchange, symbol) = holding_listing(symbols, &h);
            let last = ltp
                .get(&ltp_key(&exchange, &symbol))
                .copied()
                .unwrap_or(0.0);
            let avg = h.avg_cost_price;
            let qty = h.total_qty;
            let (pnl, pct) = if last > 0.0 && avg > 0.0 {
                (
                    py_round((last - avg) * qty as f64, 2),
                    py_round((last - avg) / avg * 100.0, 2),
                )
            } else {
                (0.0, 0.0)
            };
            Holding {
                symbol,
                exchange,
                product: "CNC".into(),
                isin: non_empty(&h.isin),
                quantity: clamp_i32(qty),
                t1_quantity: clamp_i32(h.t1_qty),
                average_price: py_round(avg, 2),
                ltp: py_round(last, 2),
                close_price: 0.0,
                pnl,
                pnl_percentage: pct,
                // web values an unpriced holding at cost (zero P&L).
                current_value: if last > 0.0 { last } else { avg } * qty as f64,
            }
        })
        .collect()
}
