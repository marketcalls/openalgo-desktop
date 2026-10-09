//! OpenAlgo <-> Upstox constants and book normalisers (web
//! `mapping/transform_data.py`, `mapping/order_data.py`,
//! `streaming/upstox_mapping.py`).
//!
//! Every book row leaves here with the OpenAlgo symbol, resolved by the
//! instrument key (`SymToken.token` holds Upstox's `NSE_EQ|INE...` key) on
//! the row's OpenAlgo exchange, exactly like the web's
//! `get_symbol(instrument_token, exchange)`.

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Master-contract `segment` -> OpenAlgo exchange (web
/// `process_upstox_json.exchange_map`). `NSE_COM` and unknown segments have
/// no OpenAlgo exchange.
pub fn exchange_from_segment(segment: &str) -> Option<&'static str> {
    Some(match segment {
        "NSE_EQ" => "NSE",
        "NSE_FO" => "NFO",
        "NCD_FO" => "CDS",
        "NSE_INDEX" => "NSE_INDEX",
        "BSE_INDEX" => "BSE_INDEX",
        "BSE_EQ" => "BSE",
        "BSE_FO" => "BFO",
        "BCD_FO" => "BCD",
        "MCX_FO" => "MCX",
        "GLOBAL_INDEX" | "GLOBAL_INDICATOR" => "GLOBAL_INDEX",
        _ => return None,
    })
}

/// Upstox segment code -> OpenAlgo exchange, for the GTT book (web
/// `UpstoxExchangeMapper.get_openalgo_exchange`, default `NSE`). The master
/// spellings (`NCD_FO`, `BCD_FO`) are accepted too.
pub fn openalgo_exchange(upstox: &str) -> &'static str {
    match upstox {
        "NSE_EQ" => "NSE",
        "NSE_FO" => "NFO",
        "NSE_CD" | "NCD_FO" => "CDS",
        "BSE_EQ" => "BSE",
        "BSE_FO" => "BFO",
        "BCD_FO" => "BCD",
        "MCX_FO" => "MCX",
        "NSE_INDEX" => "NSE_INDEX",
        "BSE_INDEX" => "BSE_INDEX",
        _ => "NSE",
    }
}

/// web `map_product_type`: CNC/NRML -> `D`, MIS -> `I`.
pub fn product_code(p: Product) -> &'static str {
    match p {
        Product::Cnc | Product::Nrml => "D",
        Product::Mis => "I",
    }
}

/// web `reverse_map_product_type(exchange, product)`.
pub fn reverse_product(exchange: &str, product: &str) -> Option<&'static str> {
    match product {
        "I" => Some("MIS"),
        "D" => match exchange {
            "NSE" | "BSE" => Some("CNC"),
            "NFO" | "BFO" | "MCX" | "CDS" => Some("NRML"),
            _ => None,
        },
        _ => None,
    }
}

/// Product shown in the books (web `map_order_data`): `D` on cash is CNC,
/// `I` is MIS, `D` on derivatives is NRML; anything else passes through.
pub fn book_product(exchange: &str, product: &str) -> String {
    match (exchange, product) {
        ("NSE" | "BSE", "D") => "CNC".into(),
        (_, "I") => "MIS".into(),
        ("NFO" | "MCX" | "BFO" | "CDS", "D") => "NRML".into(),
        _ => product.to_string(),
    }
}

/// web `map_order_type`: the four OpenAlgo price types are Upstox's own.
pub fn order_type(p: PriceType) -> &'static str {
    p.as_str()
}

const OPEN_STATUSES: &[&str] = &[
    "OPEN",
    "OPEN PENDING",
    "TRIGGER PENDING",
    "VALIDATION PENDING",
    "MODIFY PENDING",
    "MODIFY VALIDATION PENDING",
    "CANCEL PENDING",
    "MODIFIED",
    "NOT MODIFIED",
    "NOT CANCELLED",
    "PUT ORDER REQ RECEIVED",
    "AFTER MARKET ORDER REQ RECEIVED",
    "MODIFY AFTER MARKET ORDER REQ RECEIVED",
];

/// web `normalize_order_status`: every live state (including
/// `trigger pending`) reads `open`; unknown values are lowercased.
pub fn normalize_status(raw: &str) -> String {
    let s = raw.trim().to_ascii_uppercase().replace('_', " ");
    match s.as_str() {
        "COMPLETE" => "complete".into(),
        "REJECTED" => "rejected".into(),
        "CANCELLED" | "CANCELED" | "CANCELLED AFTER MARKET ORDER" => "cancelled".into(),
        x if OPEN_STATUSES.contains(&x) => "open".into(),
        _ => s.to_ascii_lowercase(),
    }
}

/// Order-update stream status map (web `upstox_order_adapter._STATUS_MAP`),
/// which keeps `trigger pending` distinct.
pub fn order_update_status(raw: &str) -> String {
    let s = raw.to_ascii_lowercase();
    match s.as_str() {
        "complete" | "cancelled" | "rejected" | "open" | "trigger pending" => s,
        "put order req received" | "modified" | "modify pending" | "cancel pending" => {
            "open".into()
        }
        "" => "open".into(),
        _ => s,
    }
}

/// `market_protection` for the v3 order APIs (web `map_market_protection`):
/// kept only when it is -1 or 1..=25.
pub fn market_protection(v: Option<i64>) -> Option<i64> {
    v.filter(|m| *m == -1 || (1..=25).contains(m))
}

// ---------------------------------------------------------------------------
// Envelope and errors
// ---------------------------------------------------------------------------

/// `errors[]` of an Upstox error body flattened like the web's
/// `_extract_error`: `CODE: message | CODE: message`. Both `errorCode` and
/// `error_code` spellings are read.
pub fn error_text(body: &Value) -> Option<String> {
    let errors = body.get("errors").and_then(Value::as_array)?;
    let parts: Vec<String> = errors
        .iter()
        .map(|e| {
            let msg = e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Unknown error");
            let code = e
                .get("errorCode")
                .or_else(|| e.get("error_code"))
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty());
            match code {
                Some(c) => format!("{}: {}", c, msg),
                None => msg.to_string(),
            }
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" | "))
}

/// First Upstox error code in a body.
pub fn error_code(body: &Value) -> Option<String> {
    body.get("errors")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|e| {
            e.get("errorCode")
                .or_else(|| e.get("error_code"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// Order id of a v3 order response (web `_extract_order_id`): place returns
/// `order_ids` (a sliced order has one id per slice; the first is used),
/// modify and cancel return `order_id`.
pub fn extract_order_id(data: &Value) -> Option<String> {
    let as_text = |v: &Value| match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    match data.get("order_ids") {
        Some(Value::Array(ids)) => return ids.first().and_then(as_text),
        Some(v) if !v.is_null() => {
            if let Some(s) = as_text(v) {
                return Some(s);
            }
        }
        _ => {}
    }
    data.get("order_id").and_then(as_text)
}

// ---------------------------------------------------------------------------
// Raw rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpstoxOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange_order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub instrument_token: String,
    /// Upstox sends both `trading_symbol` and the older `tradingsymbol`.
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub validity: String,
    #[serde(deserialize_with = "string_lenient")]
    pub status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub status_message: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub filled_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub pending_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub trigger_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub order_timestamp: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange_timestamp: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpstoxTrade {
    #[serde(deserialize_with = "string_lenient")]
    pub order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trade_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub instrument_token: String,
    /// Upstox sends both `trading_symbol` and the older `tradingsymbol`.
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub order_timestamp: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpstoxPosition {
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub instrument_token: String,
    /// Upstox sends both `trading_symbol` and the older `tradingsymbol`.
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub overnight_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub buy_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub day_buy_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub sell_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub day_sell_price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub day_buy_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub day_sell_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub buy_value: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub sell_value: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pnl: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub realised: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub unrealised: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_price: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpstoxHolding {
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub instrument_token: String,
    /// Upstox sends both `trading_symbol` and the older `tradingsymbol`.
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub t1_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub close_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pnl: f64,
}

/// The broker symbol of a row: `trading_symbol`, else `tradingsymbol`.
pub fn br_symbol<'a>(trading_symbol: &'a str, tradingsymbol: &'a str) -> &'a str {
    if trading_symbol.is_empty() {
        tradingsymbol
    } else {
        trading_symbol
    }
}

// ---------------------------------------------------------------------------
// Normalisers
// ---------------------------------------------------------------------------

fn clamp_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn non_empty(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.to_string())
}

pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// OpenAlgo symbol of a book row: by instrument key on the row's exchange
/// (web `get_symbol`), then by broker symbol, else the broker symbol as is.
pub fn oa_symbol(
    symbols: &SymbolResolver,
    instrument_token: &str,
    exchange: &str,
    tradingsymbol: &str,
) -> String {
    if !instrument_token.is_empty() {
        if let Some(r) = symbols.by_token(exchange, instrument_token) {
            return r.symbol;
        }
    }
    symbols.oa_symbol_or_raw(tradingsymbol, exchange)
}

pub fn map_orders(rows: Vec<UpstoxOrder>, symbols: &SymbolResolver) -> Vec<Order> {
    rows.into_iter()
        .map(|o| {
            let status = normalize_status(&o.status);
            Order {
                order_tag: None,
                symbol: oa_symbol(
                    symbols,
                    &o.instrument_token,
                    &o.exchange,
                    br_symbol(&o.trading_symbol, &o.tradingsymbol),
                ),
                product: book_product(&o.exchange, &o.product),
                exchange_order_id: non_empty(&o.exchange_order_id),
                exchange: o.exchange,
                side: o.transaction_type,
                quantity: clamp_i32(o.quantity),
                filled_quantity: clamp_i32(o.filled_quantity),
                pending_quantity: clamp_i32(o.pending_quantity),
                price: o.price,
                trigger_price: o.trigger_price,
                average_price: o.average_price,
                order_type: o.order_type,
                rejection_reason: if status == "rejected" {
                    non_empty(&o.status_message)
                } else {
                    None
                },
                status,
                validity: if o.validity.is_empty() {
                    "DAY".into()
                } else {
                    o.validity
                },
                order_id: o.order_id,
                order_timestamp: o.order_timestamp,
                exchange_timestamp: non_empty(&o.exchange_timestamp),
            }
        })
        .collect()
}

/// Order-book statistics (web `calculate_order_statistics`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrderStats {
    pub total_buy_orders: usize,
    pub total_sell_orders: usize,
    pub total_completed_orders: usize,
    pub total_open_orders: usize,
    pub total_rejected_orders: usize,
}

pub fn order_stats(orders: &[Order]) -> OrderStats {
    let mut s = OrderStats::default();
    for o in orders {
        match o.side.as_str() {
            "BUY" => s.total_buy_orders += 1,
            "SELL" => s.total_sell_orders += 1,
            _ => {}
        }
        match o.status.as_str() {
            "complete" => s.total_completed_orders += 1,
            "open" => s.total_open_orders += 1,
            "rejected" => s.total_rejected_orders += 1,
            _ => {}
        }
    }
    s
}

pub fn map_trades(rows: Vec<UpstoxTrade>, symbols: &SymbolResolver) -> Vec<Trade> {
    rows.into_iter()
        .map(|t| Trade {
            order_tag: None,
            symbol: oa_symbol(
                symbols,
                &t.instrument_token,
                &t.exchange,
                br_symbol(&t.trading_symbol, &t.tradingsymbol),
            ),
            product: book_product(&t.exchange, &t.product),
            exchange: t.exchange,
            side: t.transaction_type,
            quantity: clamp_i32(t.quantity),
            average_price: t.average_price,
            trade_value: t.quantity as f64 * t.average_price,
            order_id: t.order_id,
            trade_id: t.trade_id,
            timestamp: t.order_timestamp,
        })
        .collect()
}

/// Average price of a position (web `transform_positions_data`): Upstox
/// can send `average_price` null or 0, so a long falls back to `buy_price`
/// then `day_buy_price`, a short to `sell_price` then `day_sell_price`.
pub fn position_average(p: &UpstoxPosition) -> f64 {
    if p.average_price != 0.0 {
        return p.average_price;
    }
    let pick = |a: f64, b: f64| if a != 0.0 { a } else { b };
    match p.quantity {
        q if q > 0 => pick(p.buy_price, p.day_buy_price),
        q if q < 0 => pick(p.sell_price, p.day_sell_price),
        _ => 0.0,
    }
}

pub fn map_positions(rows: Vec<UpstoxPosition>, symbols: &SymbolResolver) -> Vec<Position> {
    rows.into_iter()
        .map(|p| Position {
            symbol: oa_symbol(
                symbols,
                &p.instrument_token,
                &p.exchange,
                br_symbol(&p.trading_symbol, &p.tradingsymbol),
            ),
            product: book_product(&p.exchange, &p.product),
            average_price: position_average(&p),
            exchange: p.exchange,
            quantity: clamp_i32(p.quantity),
            overnight_quantity: clamp_i32(p.overnight_quantity),
            ltp: p.last_price,
            pnl: p.pnl,
            realized_pnl: p.realised,
            unrealized_pnl: p.unrealised,
            buy_quantity: clamp_i32(p.day_buy_quantity),
            buy_value: p.buy_value,
            sell_quantity: clamp_i32(p.day_sell_quantity),
            sell_value: p.sell_value,
        })
        .collect()
}

pub fn map_holdings(rows: Vec<UpstoxHolding>, symbols: &SymbolResolver) -> Vec<Holding> {
    rows.into_iter()
        .map(|h| {
            if h.product != "D" && !h.product.is_empty() {
                tracing::debug!("Upstox holding with product {}", h.product);
            }
            let pnl_percentage = if h.average_price == 0.0 {
                0.0
            } else {
                round2((h.last_price - h.average_price) / h.average_price * 100.0)
            };
            Holding {
                symbol: oa_symbol(
                    symbols,
                    &h.instrument_token,
                    &h.exchange,
                    br_symbol(&h.trading_symbol, &h.tradingsymbol),
                ),
                product: if h.product == "D" || h.product.is_empty() {
                    "CNC".into()
                } else {
                    h.product.clone()
                },
                isin: non_empty(&h.isin),
                quantity: clamp_i32(h.quantity),
                t1_quantity: clamp_i32(h.t1_quantity),
                average_price: h.average_price,
                ltp: h.last_price,
                close_price: h.close_price,
                pnl: round2(h.pnl),
                pnl_percentage,
                current_value: h.last_price * h.quantity as f64,
                exchange: h.exchange,
            }
        })
        .collect()
}

/// Whether a raw order is one cancel-all touches (web: the raw lowercase
/// statuses `open` and `trigger pending`).
pub fn is_cancellable_raw(status: &str) -> bool {
    matches!(status, "open" | "trigger pending")
}

/// The OpenAlgo exchange enum for a book row, when it is one.
pub fn parse_exchange(s: &str) -> Option<Exchange> {
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn segments_map_like_the_master_contract() {
        assert_eq!(exchange_from_segment("NSE_EQ"), Some("NSE"));
        assert_eq!(exchange_from_segment("NCD_FO"), Some("CDS"));
        assert_eq!(exchange_from_segment("BCD_FO"), Some("BCD"));
        assert_eq!(
            exchange_from_segment("GLOBAL_INDICATOR"),
            Some("GLOBAL_INDEX")
        );
        assert_eq!(exchange_from_segment("NSE_COM"), None);
        assert_eq!(openalgo_exchange("NSE_FO"), "NFO");
        assert_eq!(openalgo_exchange("NSE_CD"), "CDS");
        assert_eq!(openalgo_exchange("XYZ"), "NSE");
    }

    #[test]
    fn products() {
        assert_eq!(product_code(Product::Cnc), "D");
        assert_eq!(product_code(Product::Nrml), "D");
        assert_eq!(product_code(Product::Mis), "I");
        assert_eq!(reverse_product("NSE", "D"), Some("CNC"));
        assert_eq!(reverse_product("MCX", "D"), Some("NRML"));
        assert_eq!(reverse_product("BCD", "D"), None);
        assert_eq!(reverse_product("NFO", "I"), Some("MIS"));
        assert_eq!(book_product("BSE", "D"), "CNC");
        assert_eq!(book_product("NFO", "D"), "NRML");
        assert_eq!(book_product("NFO", "I"), "MIS");
        assert_eq!(book_product("BCD", "D"), "D");
    }

    #[test]
    fn statuses() {
        assert_eq!(normalize_status("complete"), "complete");
        assert_eq!(normalize_status("trigger pending"), "open");
        assert_eq!(normalize_status("put order req received"), "open");
        assert_eq!(normalize_status("not_cancelled"), "open");
        assert_eq!(
            normalize_status("cancelled after market order"),
            "cancelled"
        );
        assert_eq!(normalize_status("canceled"), "cancelled");
        assert_eq!(normalize_status("rejected"), "rejected");
        assert_eq!(normalize_status("Something New"), "something new");
        assert_eq!(order_update_status("trigger pending"), "trigger pending");
        assert_eq!(order_update_status("modify pending"), "open");
        assert_eq!(order_update_status(""), "open");
        assert_eq!(
            order_update_status("after market order req received"),
            "after market order req received"
        );
    }

    #[test]
    fn market_protection_range() {
        assert_eq!(market_protection(Some(-1)), Some(-1));
        assert_eq!(market_protection(Some(25)), Some(25));
        assert_eq!(market_protection(Some(0)), None);
        assert_eq!(market_protection(Some(26)), None);
        assert_eq!(market_protection(None), None);
    }

    #[test]
    fn errors_and_ids() {
        let body = json!({"status": "error", "errors": [
            {"errorCode": "UDAPI100050", "message": "Invalid order"},
            {"error_code": "UDAPI1", "message": "Second"}
        ]});
        assert_eq!(
            error_text(&body).unwrap(),
            "UDAPI100050: Invalid order | UDAPI1: Second"
        );
        assert_eq!(error_code(&body).unwrap(), "UDAPI100050");
        assert_eq!(error_text(&json!({"status": "error"})), None);
        assert_eq!(
            extract_order_id(&json!({"order_ids": ["1", "2"]})).unwrap(),
            "1"
        );
        assert_eq!(extract_order_id(&json!({"order_ids": "9"})).unwrap(), "9");
        assert_eq!(extract_order_id(&json!({"order_id": "7"})).unwrap(), "7");
        assert_eq!(extract_order_id(&json!({"order_ids": []})), None);
    }

    #[test]
    fn position_average_falls_back_by_side() {
        let p = |q, avg, b, db, s, ds| UpstoxPosition {
            quantity: q,
            average_price: avg,
            buy_price: b,
            day_buy_price: db,
            sell_price: s,
            day_sell_price: ds,
            ..Default::default()
        };
        assert_eq!(position_average(&p(10, 5.0, 1.0, 2.0, 3.0, 4.0)), 5.0);
        assert_eq!(position_average(&p(10, 0.0, 1.0, 2.0, 3.0, 4.0)), 1.0);
        assert_eq!(position_average(&p(10, 0.0, 0.0, 2.0, 3.0, 4.0)), 2.0);
        assert_eq!(position_average(&p(-10, 0.0, 1.0, 2.0, 0.0, 4.0)), 4.0);
        assert_eq!(position_average(&p(0, 0.0, 1.0, 2.0, 3.0, 4.0)), 0.0);
    }
}
