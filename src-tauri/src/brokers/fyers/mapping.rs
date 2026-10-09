//! Fyers <-> OpenAlgo translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `mapping/margin_data.py`).
//!
//! Books come back with Fyers symbols (`NSE:SBIN-EQ`) and numeric
//! exchange/segment codes; every normaliser maps the code pair to the
//! OpenAlgo exchange and the Fyers symbol back to the OpenAlgo symbol
//! (web `get_oa_symbol`), keeping the Fyers symbol when the master has no
//! row, as the web does.

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde::Deserialize;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Constants (web transform_data.py / order_data.py)
// ---------------------------------------------------------------------------

/// `(exchange, segment)` -> OpenAlgo exchange (web `exchange_map`).
pub fn exchange_name(exchange: i64, segment: i64) -> Option<&'static str> {
    match (exchange, segment) {
        (10, 10) => Some("NSE"),
        (10, 11) => Some("NFO"),
        (10, 12) => Some("CDS"),
        (12, 10) => Some("BSE"),
        (12, 11) => Some("BFO"),
        (11, 20) => Some("MCX"),
        _ => None,
    }
}

/// web `get_exchange`: unknown pairs read "Unknown Exchange".
pub fn get_exchange(exchange: i64, segment: i64) -> String {
    exchange_name(exchange, segment)
        .unwrap_or("Unknown Exchange")
        .to_string()
}

/// web `map_order_type`: unknown -> MARKET (2).
pub fn order_type_code(p: PriceType) -> i64 {
    match p {
        PriceType::Market => 2,
        PriceType::Limit => 1,
        PriceType::Sl => 4,
        PriceType::SlM => 3,
    }
}

/// web `map_action`.
pub fn side_code(a: Action) -> i64 {
    match a {
        Action::Buy => 1,
        Action::Sell => -1,
    }
}

/// web `map_product_type`.
pub fn product_code(p: Product) -> &'static str {
    match p {
        Product::Cnc => "CNC",
        Product::Nrml => "MARGIN",
        Product::Mis => "INTRADAY",
    }
}

/// web `reverse_map_product_type` / `product_map`: unknown -> `unknown`.
pub fn oa_product(fyers: &str) -> String {
    match fyers {
        "CNC" => "CNC",
        "INTRADAY" => "MIS",
        "MARGIN" => "NRML",
        "CO" => "CO",
        "BO" => "BO",
        _ => "unknown",
    }
    .to_string()
}

/// web `status_map`: 1 cancelled, 2 complete, 4 trigger pending, 5 rejected,
/// 6 open, anything else `unknown`.
pub fn order_status(code: i64) -> String {
    match code {
        1 => "cancelled",
        2 => "complete",
        4 => "trigger pending",
        5 => "rejected",
        6 => "open",
        _ => "unknown",
    }
    .to_string()
}

/// web `side_map`.
pub fn side_name(code: i64) -> String {
    match code {
        1 => "BUY",
        -1 => "SELL",
        _ => "unknown",
    }
    .to_string()
}

/// web `type_map`.
pub fn pricetype_name(code: i64) -> String {
    match code {
        1 => "LIMIT",
        2 => "MARKET",
        3 => "SL-M",
        4 => "SL",
        _ => "unknown",
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

/// `POST /api/v3/orders/sync` body (web `transform_data`): validity is
/// always DAY, no AMO, the `openalgo` order tag.
pub fn place_order_body(o: &ResolvedOrder) -> Value {
    json!({
        "symbol": o.brsymbol(),
        "qty": o.quantity,
        "type": order_type_code(o.pricetype),
        "side": side_code(o.action),
        "productType": product_code(o.product),
        "limitPrice": o.price,
        "stopPrice": o.trigger_price,
        "validity": "DAY",
        "disclosedQty": o.disclosed_quantity,
        "offlineOrder": false,
        "stopLoss": 0,
        "takeProfit": 0,
        "orderTag": "openalgo",
    })
}

/// `PATCH /api/v3/orders/sync` body (web `transform_modify_order_data`):
/// all five fields always present.
pub fn modify_order_body(m: &ResolvedModify) -> Value {
    json!({
        "id": m.order_id,
        "qty": m.quantity,
        "type": order_type_code(m.pricetype),
        "limitPrice": m.price,
        "stopPrice": m.trigger_price,
    })
}

/// One `multiorder/margin` entry (web `transform_margin_positions`), or
/// `None` when the symbol does not resolve.
pub fn margin_leg(leg: &MarginLeg, symbols: &SymbolResolver) -> Option<Value> {
    let br = symbols
        .br_symbol(&leg.key.symbol, &leg.key.exchange)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none"))?;
    Some(json!({
        "symbol": br,
        "qty": leg.quantity,
        "side": side_code(leg.action),
        "type": order_type_code(leg.pricetype),
        "productType": product_code(leg.product),
        "limitPrice": leg.price,
        "stopLoss": 0.0,
        "stopPrice": leg.trigger_price,
        "takeProfit": 0.0,
    }))
}

/// web `parse_margin_response`: Fyers reports only the total
/// (`margin_new_order`); span and exposure are 0.
pub fn parse_margin(v: &Value) -> MarginResult {
    let f = |k: &str| v.pointer(&format!("/data/{}", k)).map(num).unwrap_or(0.0);
    MarginResult {
        total_margin_required: f("margin_new_order"),
        span_margin: 0.0,
        exposure_margin: 0.0,
    }
}

pub(crate) fn num(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

// ---------------------------------------------------------------------------
// Fyers book rows (null tolerant)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FyersOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exch_ord_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub exchange: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub segment: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub side: i64,
    #[serde(rename = "type", deserialize_with = "i64_lenient")]
    pub kind: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub status: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub qty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub filled_qty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub remaining_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub limit_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub stop_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub traded_price: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub product_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_validity: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_date_time: String,
    #[serde(deserialize_with = "string_lenient")]
    pub message: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FyersTrade {
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub exchange: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub segment: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub side: i64,
    #[serde(deserialize_with = "string_lenient")]
    pub product_type: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub traded_qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub trade_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub trade_value: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub order_number: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trade_number: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_date_time: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FyersPosition {
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub exchange: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub segment: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub net_qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub net_avg: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pl: f64,
    #[serde(rename = "realized_profit", deserialize_with = "f64_lenient")]
    pub realized_profit: f64,
    #[serde(rename = "unrealized_profit", deserialize_with = "f64_lenient")]
    pub unrealized_profit: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub buy_qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub buy_val: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub sell_qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub sell_val: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub cf_buy_qty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub cf_sell_qty: i64,
    #[serde(deserialize_with = "string_lenient")]
    pub product_type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FyersHolding {
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub exchange: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub segment: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub cost_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pl: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
    #[serde(deserialize_with = "string_lenient")]
    pub holding_type: String,
}

/// Rows under `key` of a Fyers book body (`orderBook`, `tradeBook`,
/// `netPositions`, `holdings`); a missing or null list is empty, and a row
/// that does not parse is skipped rather than failing the book.
pub fn rows<T: serde::de::DeserializeOwned>(v: &Value, key: &str) -> Vec<T> {
    match v.get(key) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|r| match serde_json::from_value::<T>(r.clone()) {
                Ok(x) => Some(x),
                Err(e) => {
                    tracing::warn!("Skipping an unreadable Fyers {} row: {}", key, e);
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Normalisers
// ---------------------------------------------------------------------------

/// `(OpenAlgo symbol, OpenAlgo exchange)` of a Fyers book row (web
/// `map_order_data` and friends).
pub fn oa_identity(
    brsymbol: &str,
    exchange: i64,
    segment: i64,
    symbols: &SymbolResolver,
) -> (String, String) {
    let ex = get_exchange(exchange, segment);
    if brsymbol.is_empty() {
        return (String::new(), ex);
    }
    match symbols.oa_symbol(brsymbol, &ex) {
        Some(s) => (s, ex),
        None => {
            tracing::warn!(
                "Could not map Fyers symbol {} on {}; keeping it",
                brsymbol,
                ex
            );
            (brsymbol.to_string(), ex)
        }
    }
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn non_empty(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.to_string())
}

pub fn round2(v: f64) -> f64 {
    crate::brokers::common::mpp::py_round(v, 2)
}

/// web `transform_order_data`.
pub fn map_orders(rows: Vec<FyersOrder>, symbols: &SymbolResolver) -> Vec<Order> {
    rows.into_iter()
        .map(|o| {
            let (symbol, exchange) = oa_identity(&o.symbol, o.exchange, o.segment, symbols);
            let status = order_status(o.status);
            if status == "unknown" {
                tracing::warn!("Unknown Fyers order status {} for {}", o.status, o.id);
            }
            let pending = if o.remaining_quantity > 0 {
                o.remaining_quantity
            } else {
                (o.qty - o.filled_qty).max(0)
            };
            Order {
                order_tag: None,
                order_id: o.id,
                exchange_order_id: non_empty(&o.exch_ord_id),
                symbol,
                exchange,
                side: side_name(o.side),
                quantity: clamp_i32(o.qty),
                filled_quantity: clamp_i32(o.filled_qty),
                pending_quantity: clamp_i32(if status == "open" || status == "trigger pending" {
                    pending
                } else {
                    0
                }),
                price: o.limit_price,
                trigger_price: o.stop_price,
                average_price: o.traded_price,
                order_type: pricetype_name(o.kind),
                product: oa_product(&o.product_type),
                rejection_reason: (status == "rejected")
                    .then(|| non_empty(&o.message))
                    .flatten(),
                status,
                validity: if o.order_validity.is_empty() {
                    "DAY".into()
                } else {
                    o.order_validity
                },
                order_timestamp: o.order_date_time,
                exchange_timestamp: None,
            }
        })
        .collect()
}

/// web `transform_tradebook_data`.
pub fn map_trades(rows: Vec<FyersTrade>, symbols: &SymbolResolver) -> Vec<Trade> {
    rows.into_iter()
        .map(|t| {
            let (symbol, exchange) = oa_identity(&t.symbol, t.exchange, t.segment, symbols);
            Trade {
                order_tag: None,
                order_id: t.order_number,
                trade_id: t.trade_number,
                symbol,
                exchange,
                product: oa_product(&t.product_type),
                side: side_name(t.side),
                quantity: clamp_i32(t.traded_qty),
                average_price: t.trade_price,
                trade_value: t.trade_value,
                timestamp: t.order_date_time,
            }
        })
        .collect()
}

/// web `transform_positions_data`: average, LTP and P&L rounded to 2.
pub fn map_positions(rows: Vec<FyersPosition>, symbols: &SymbolResolver) -> Vec<Position> {
    rows.into_iter()
        .map(|p| {
            let (symbol, exchange) = oa_identity(&p.symbol, p.exchange, p.segment, symbols);
            Position {
                symbol,
                exchange,
                product: oa_product(&p.product_type),
                quantity: clamp_i32(p.net_qty),
                overnight_quantity: clamp_i32(p.cf_buy_qty - p.cf_sell_qty),
                average_price: round2(p.net_avg),
                ltp: round2(p.ltp),
                pnl: round2(p.pl),
                realized_pnl: p.realized_profit,
                unrealized_pnl: p.unrealized_profit,
                buy_quantity: clamp_i32(p.buy_qty),
                buy_value: p.buy_val,
                sell_quantity: clamp_i32(p.sell_qty),
                sell_value: p.sell_val,
            }
        })
        .collect()
}

/// web `map_portfolio_data` + `transform_holdings_data`: `HLD`/`T1` become
/// CNC; P&L % is `(ltp - cost) / cost * 100`, 0 when cost is 0.
pub fn map_holdings(rows: Vec<FyersHolding>, symbols: &SymbolResolver) -> Vec<Holding> {
    rows.into_iter()
        .map(|h| {
            let (symbol, exchange) = oa_identity(&h.symbol, h.exchange, h.segment, symbols);
            let product = if h.holding_type == "HLD" || h.holding_type == "T1" {
                "CNC".to_string()
            } else {
                tracing::warn!("Unknown Fyers holding type {}", h.holding_type);
                h.holding_type.clone()
            };
            let pnl_percentage = if h.cost_price != 0.0 {
                round2((h.ltp - h.cost_price) / h.cost_price * 100.0)
            } else {
                0.0
            };
            Holding {
                symbol,
                exchange,
                product,
                isin: non_empty(&h.isin),
                quantity: clamp_i32(h.quantity),
                t1_quantity: if h.holding_type == "T1" {
                    clamp_i32(h.quantity)
                } else {
                    0
                },
                average_price: round2(h.cost_price),
                ltp: round2(h.ltp),
                close_price: 0.0,
                pnl: round2(h.pl),
                pnl_percentage,
                current_value: h.ltp * h.quantity as f64,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_tables_match_web() {
        assert_eq!(exchange_name(10, 11), Some("NFO"));
        assert_eq!(exchange_name(11, 20), Some("MCX"));
        assert_eq!(get_exchange(99, 1), "Unknown Exchange");
        assert_eq!(order_type_code(PriceType::SlM), 3);
        assert_eq!(order_type_code(PriceType::Sl), 4);
        assert_eq!(side_code(Action::Sell), -1);
        assert_eq!(product_code(Product::Nrml), "MARGIN");
        assert_eq!(oa_product("INTRADAY"), "MIS");
        assert_eq!(oa_product("MTF"), "unknown");
        assert_eq!(order_status(4), "trigger pending");
        assert_eq!(order_status(7), "unknown");
        assert_eq!(pricetype_name(3), "SL-M");
        assert_eq!(side_name(0), "unknown");
    }

    #[test]
    fn margin_response_uses_new_order_total() {
        let v = json!({"s":"ok","code":200,"data":{"margin_avail":1999.9,"margin_total":147738.0563,"margin_new_order":147738.0563}});
        let m = parse_margin(&v);
        assert_eq!(m.total_margin_required, 147738.0563);
        assert_eq!((m.span_margin, m.exposure_margin), (0.0, 0.0));
    }
}
