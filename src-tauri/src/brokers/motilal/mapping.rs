//! OpenAlgo <-> Motilal vocabulary and book normalisers (web
//! `mapping/transform_data.py`, `mapping/order_data.py`).

use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::mpp::py_round;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Lenient JSON readers (Motilal sends numbers as numbers or strings)
// ---------------------------------------------------------------------------

/// A non-empty string field (numbers are stringified).
pub fn vs(v: &Value, k: &str) -> Option<String> {
    match v.get(k)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// web `_to_float` (0 on anything unparsable).
pub fn vf(v: &Value, k: &str) -> f64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// web `_to_int` (`int(float(x))`).
pub fn vi(v: &Value, k: &str) -> i64 {
    vf(v, k) as i64
}

/// Token text: Motilal sends `symboltoken` as a JSON number; the master
/// stores strings (`order_data.py:155-159`).
pub fn token_text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|i| i.to_string())
            .unwrap_or_else(|| n.to_string()),
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Exchanges, products, order types (transform_data.py)
// ---------------------------------------------------------------------------

/// web `map_exchange` (OpenAlgo -> Motilal).
pub fn map_exchange(exchange: &str) -> &str {
    match exchange {
        "NFO" => "NSEFO",
        "CDS" => "NSECD",
        "BFO" => "BSEFO",
        other => other,
    }
}

/// web `reverse_map_exchange` (Motilal -> OpenAlgo).
pub fn reverse_map_exchange(exchange: &str) -> &str {
    match exchange {
        "NSEFO" => "NFO",
        "NSECD" => "CDS",
        "BSEFO" => "BFO",
        other => other,
    }
}

const FO: &[&str] = &[
    "NFO", "MCX", "CDS", "BFO", "NCDEX", "NSEFO", "NSECD", "BSEFO",
];

/// web `map_product_type(product, exchange)`: exchange-aware; every product
/// is `NORMAL` on derivatives.
pub fn map_product_type(product: &str, exchange: &str) -> &'static str {
    let p = product.to_ascii_uppercase();
    let e = exchange.to_ascii_uppercase();
    if FO.contains(&e.as_str()) {
        return "NORMAL";
    }
    // Cash (NSE/BSE) and an unknown exchange share one table on the web.
    match p.as_str() {
        "CNC" => "DELIVERY",
        "NRML" => "NORMAL",
        _ => "VALUEPLUS",
    }
}

/// web `reverse_map_product_type` (unknown -> MIS).
pub fn reverse_map_product_type(product: &str) -> &'static str {
    match product.trim().to_ascii_uppercase().as_str() {
        "DELIVERY" | "SELLFROMDP" | "BTST" => "CNC",
        "NORMAL" | "MTF" => "NRML",
        _ => "MIS",
    }
}

/// web `map_order_type`.
pub fn map_order_type(pricetype: PriceType) -> &'static str {
    match pricetype {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl | PriceType::SlM => "STOPLOSS",
    }
}

/// Book order type -> OpenAlgo (`order_data.py:276-278`): STOPLOSS splits on
/// the trigger.
pub fn oa_pricetype(ordertype: &str, trigger_price: f64) -> String {
    let t = ordertype.trim().to_ascii_uppercase();
    if t == "STOPLOSS" {
        if trigger_price > 0.0 { "SL" } else { "SL-M" }.to_string()
    } else {
        t
    }
}

/// `CASH` or `DERIVATIVES` segment of an OpenAlgo exchange (`data.py`).
pub fn segment(exchange: &str) -> &'static str {
    if matches!(exchange, "NFO" | "BFO" | "CDS" | "MCX" | "NCDEX" | "NCX") {
        "DERIVATIVES"
    } else {
        "CASH"
    }
}

/// Lot-size-bearing exchanges (`order_api.py:29` `DERIVATIVE_EXCHANGES`).
pub fn is_derivative_exchange(exchange: &str) -> bool {
    matches!(
        exchange,
        "NFO" | "CDS" | "MCX" | "BFO" | "NSEFO" | "NSECD" | "BSEFO"
    )
}

/// Index pseudo-exchange -> the real exchange of the index APIs
/// (`data.py` `INDEX_EXCHANGE_MAP`; MCX indices are unsupported there).
pub fn index_exchange(exchange: &str) -> Option<&'static str> {
    match exchange {
        "NSE_INDEX" => Some("NSE"),
        "BSE_INDEX" => Some("BSE"),
        _ => None,
    }
}

pub fn is_index(exchange: &str) -> bool {
    matches!(exchange, "NSE_INDEX" | "BSE_INDEX" | "MCX_INDEX")
}

// ---------------------------------------------------------------------------
// Statuses and price scaling (order_data.py)
// ---------------------------------------------------------------------------

/// web `ORDER_STATUS_MAP`; unknown statuses read as `open`.
pub fn map_order_status(status: &str) -> &'static str {
    match status.trim().to_ascii_lowercase().as_str() {
        "traded" | "complete" => "complete",
        "rejected" | "error" => "rejected",
        "cancel" | "cancelled" => "cancelled",
        "sent" | "confirm" | "open" | "partial" | "unknown" => "open",
        other => {
            if !other.is_empty() {
                tracing::warn!(
                    "Unrecognised Motilal order status '{}'; treating as open",
                    other
                );
            }
            "open"
        }
    }
}

/// web `DEFAULT_PRECISION`.
pub const DEFAULT_PRECISION: i64 = 2;

/// web `_get_precision(record, default)`.
pub fn precision(row: &Value, default: Option<i64>) -> Option<i64> {
    match row.get("precision") {
        None | Some(Value::Null) => default,
        Some(v) => {
            let p = match v {
                Value::Number(n) => n.as_f64().map(|f| f as i64),
                Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
                _ => None,
            }
            .unwrap_or(DEFAULT_PRECISION);
            if !(0..=8).contains(&p) {
                Some(DEFAULT_PRECISION)
            } else {
                Some(p)
            }
        }
    }
}

/// web `_scale_price`.
pub fn scale(value: f64, precision: Option<i64>) -> f64 {
    match precision {
        None => value,
        Some(p) => value / 10f64.powi(p as i32),
    }
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// OpenAlgo symbol of a book row: master lookup by `symboltoken` on the
/// OpenAlgo exchange, else the row's own symbol.
pub fn oa_symbol(
    symbols: &SymbolResolver,
    row: &Value,
    oa_exchange: &str,
    fallback: &str,
) -> String {
    let token = token_text(row, "symboltoken");
    if !token.is_empty() {
        if let Some(r) = symbols.by_token(oa_exchange, &token) {
            return r.symbol;
        }
    }
    vs(row, fallback).unwrap_or_default()
}

/// web `_order_timestamp`.
pub fn order_timestamp(row: &Value) -> String {
    ["lastmodifiedtime", "entrydatetime", "recordinserttime"]
        .iter()
        .filter_map(|k| vs(row, k))
        .find(|v| v != "0")
        .unwrap_or_default()
}

/// web `map_order_data` + `transform_order_data`.
pub fn map_orders(rows: &[Value], symbols: &SymbolResolver) -> Vec<Order> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| {
            let exchange = reverse_map_exchange(&vs(r, "exchange").unwrap_or_default()).to_string();
            let p = precision(r, None);
            let trigger = scale(vf(r, "triggerprice"), p);
            let avg = scale(vf(r, "averageprice"), p);
            let price = scale(vf(r, "price"), p);
            let display = if avg > 0.0 { avg } else { price };
            let quantity = vi(r, "orderqty");
            let filled = vi(r, "qtytradedtoday").max(vi(r, "totalqtytraded"));
            let pending = r
                .get("totalqtyremaining")
                .map(|_| vi(r, "totalqtyremaining"))
                .unwrap_or_else(|| (quantity - filled).max(0));
            Order {
                order_id: vs(r, "uniqueorderid").unwrap_or_default(),
                exchange_order_id: vs(r, "exchorderid"),
                symbol: oa_symbol(symbols, r, &exchange, "symbol"),
                product: reverse_map_product_type(&vs(r, "producttype").unwrap_or_default())
                    .to_string(),
                side: vs(r, "buyorsell").unwrap_or_default().to_ascii_uppercase(),
                quantity: quantity as i32,
                filled_quantity: filled as i32,
                pending_quantity: pending as i32,
                price: py_round(display, 2),
                trigger_price: py_round(trigger, 2),
                average_price: py_round(avg, 2),
                order_type: oa_pricetype(&vs(r, "ordertype").unwrap_or_default(), trigger),
                status: map_order_status(&vs(r, "orderstatus").unwrap_or_default()).to_string(),
                validity: vs(r, "orderduration").unwrap_or_else(|| "DAY".into()),
                order_timestamp: order_timestamp(r),
                exchange_timestamp: None,
                rejection_reason: vs(r, "error"),
                exchange,
            }
        })
        .collect()
}

/// web `map_trade_data` + `transform_tradebook_data` (scale defaults to 2).
pub fn map_trades(rows: &[Value], symbols: &SymbolResolver) -> Vec<Trade> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| {
            let exchange = reverse_map_exchange(&vs(r, "exchange").unwrap_or_default()).to_string();
            let p = match precision(r, Some(DEFAULT_PRECISION)) {
                Some(p) if p > 0 => p,
                _ => DEFAULT_PRECISION,
            };
            Trade {
                order_id: vs(r, "uniqueorderid").unwrap_or_default(),
                trade_id: vs(r, "tradeno").unwrap_or_default(),
                symbol: oa_symbol(symbols, r, &exchange, "symbol"),
                product: reverse_map_product_type(&vs(r, "producttype").unwrap_or_default())
                    .to_string(),
                side: vs(r, "buyorsell").unwrap_or_default().to_ascii_uppercase(),
                quantity: vi(r, "tradeqty") as i32,
                average_price: py_round(scale(vf(r, "tradeprice"), Some(p)), 2),
                trade_value: py_round(scale(vf(r, "tradevalue"), Some(p)), 2),
                timestamp: vs(r, "tradetime").unwrap_or_default(),
                exchange,
            }
        })
        .collect()
}

/// web `map_position_data` + `transform_positions_data` (no scaling).
pub fn map_positions(rows: &[Value], symbols: &SymbolResolver) -> Vec<Position> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| {
            let exchange = reverse_map_exchange(&vs(r, "exchange").unwrap_or_default()).to_string();
            let buy_qty = vi(r, "buyquantity");
            let sell_qty = vi(r, "sellquantity");
            let net = buy_qty - sell_qty;
            let buy_amt = vf(r, "buyamount");
            let sell_amt = vf(r, "sellamount");
            let avg = if net > 0 {
                if buy_qty > 0 {
                    buy_amt / buy_qty as f64
                } else {
                    0.0
                }
            } else if net < 0 {
                if sell_qty > 0 {
                    sell_amt / sell_qty as f64
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let mtm = vf(r, "marktomarket");
            let booked = vf(r, "bookedprofitloss");
            Position {
                symbol: oa_symbol(symbols, r, &exchange, "symbol"),
                product: reverse_map_product_type(&vs(r, "productname").unwrap_or_default())
                    .to_string(),
                quantity: net as i32,
                overnight_quantity: 0,
                average_price: avg,
                ltp: vf(r, "LTP"),
                pnl: mtm + booked,
                realized_pnl: booked,
                unrealized_pnl: mtm,
                buy_quantity: buy_qty as i32,
                buy_value: buy_amt,
                sell_quantity: sell_qty as i32,
                sell_value: sell_amt,
                exchange,
            }
        })
        .collect()
}

/// web `map_portfolio_data` + `transform_holdings_data`: exchange from
/// whichever token is present (NSE first), product CNC.
pub fn map_holdings(rows: &[Value], symbols: &SymbolResolver) -> Vec<Holding> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| {
            let nse = vi(r, "nsesymboltoken");
            let bse = vi(r, "bsescripcode");
            let scripname = vs(r, "scripname").unwrap_or_default();
            let (exchange, symbol) = if nse > 0 {
                let t = nse.to_string();
                (
                    "NSE",
                    symbols
                        .by_token("NSE", &t)
                        .map(|x| x.symbol)
                        .unwrap_or_else(|| scripname.clone()),
                )
            } else if bse > 0 {
                let t = bse.to_string();
                (
                    "BSE",
                    symbols
                        .by_token("BSE", &t)
                        .map(|x| x.symbol)
                        .unwrap_or_else(|| scripname.clone()),
                )
            } else {
                ("NSE", scripname.clone())
            };
            let qty = vi(r, "dpquantity");
            let avg = vf(r, "buyavgprice");
            let ltp = vf(r, "LTP");
            let (pnl, pct) = if ltp > 0.0 && avg > 0.0 && qty != 0 {
                ((ltp - avg) * qty as f64, (ltp - avg) / avg * 100.0)
            } else {
                (0.0, 0.0)
            };
            Holding {
                symbol,
                exchange: exchange.to_string(),
                product: "CNC".into(),
                isin: vs(r, "isin"),
                quantity: qty as i32,
                t1_quantity: 0,
                average_price: py_round(avg, 2),
                ltp,
                close_price: 0.0,
                pnl: py_round(pnl, 2),
                pnl_percentage: py_round(pct, 2),
                current_value: if ltp > 0.0 {
                    ltp * qty as f64
                } else {
                    avg * qty as f64
                },
            }
        })
        .collect()
}

/// Product the web compares positions against in `get_open_position`.
pub fn position_product(product: Product, exchange: Exchange) -> &'static str {
    map_product_type(product.as_str(), exchange.as_str())
}
