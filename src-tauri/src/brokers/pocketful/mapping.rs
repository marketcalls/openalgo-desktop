//! OpenAlgo <-> Pocketful translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `streaming/pocketful_mapping.py`).
//!
//! Pocketful answers are read as loose JSON: the web reads every field with
//! fallbacks (`trading_symbol` or `tradingsymbol`, `quantity` or
//! `net_quantity`), and numbers arrive as numbers or strings.

use crate::brokers::common::mapping::{PriceType, Product};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Loose JSON readers
// ---------------------------------------------------------------------------

/// String field (numbers are printed; null and absent are empty).
pub fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// First non-empty string among `keys`.
pub fn text_any(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| text(v, k))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Number field (numeric strings accepted; 0 when absent or unreadable).
pub fn num(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(as_f64).unwrap_or(0.0)
}

/// The first of `keys` that is present and numeric.
pub fn num_any(v: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| v.get(*k).and_then(as_f64))
}

/// Integer field, truncated like Python `int(float(x))`.
pub fn int(v: &Value, key: &str) -> i64 {
    num(v, key) as i64
}

fn i32_of(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// A list at `data`, `data.<key>` or the first list inside `data`, as the
/// web's book readers accept.
pub fn list_at<'a>(body: &'a Value, key: &str) -> &'a [Value] {
    let data = &body["data"];
    if let Some(a) = data.as_array() {
        return a;
    }
    if let Some(a) = data.get(key).and_then(Value::as_array) {
        return a;
    }
    if let Some(obj) = data.as_object() {
        if let Some(a) = obj.values().find_map(Value::as_array) {
            return a;
        }
    }
    &[]
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// web `map_order_type`: only `SL-M` differs.
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "SL",
        PriceType::SlM => "SLM",
    }
}

/// Pocketful order type -> OpenAlgo price type.
pub fn oa_pricetype(s: &str) -> String {
    match s.trim().to_ascii_uppercase().as_str() {
        "SLM" | "SL-M" => "SL-M".into(),
        "MARKET" | "MKT" => "MARKET".into(),
        "LIMIT" | "LMT" => "LIMIT".into(),
        "SL" | "SL-L" => "SL".into(),
        other => other.to_string(),
    }
}

/// web `map_product_type`: identical names.
pub fn product(p: Product) -> &'static str {
    p.as_str()
}

/// web `reverse_map_product_type` (identity on CNC/NRML/MIS).
pub fn oa_product(s: &str) -> String {
    s.trim().to_ascii_uppercase()
}

/// Feed exchange codes (web `data.py:190`, `pocketful_mapping.py:9-18`);
/// unknown exchanges default to NSE (1) like the web.
pub fn exchange_code(exchange: &str) -> u8 {
    match exchange {
        "NSE" | "NSE_INDEX" => 1,
        "NFO" => 2,
        "CDS" => 3,
        "MCX" => 4,
        "BSE" | "BSE_INDEX" => 6,
        "BFO" => 7,
        _ => 1,
    }
}

/// Order status (web `transform_order_data`): substring tests on the upper
/// `order_status`, in the web's order, with `mode == NEW` meaning open.
pub fn map_status(status: &str, mode: &str) -> String {
    let s = status.trim().to_ascii_uppercase();
    let mode = mode.trim().to_ascii_uppercase();
    if s.contains("CANCEL_CONFIRMED") {
        "cancelled".into()
    } else if s.contains("COMPLETE") {
        "complete".into()
    } else if s.contains("REJECTED") {
        "rejected".into()
    } else if s.contains("TRIGGER PENDING") {
        "trigger pending".into()
    } else if s.contains("OPEN")
        || s.contains("PENDING")
        || s.contains("AMO_SUBMIT")
        || s.contains("MODIFY")
        || mode == "NEW"
    {
        "open".into()
    } else if s.contains("CANCEL") {
        "cancelled".into()
    } else if s.is_empty() {
        "unknown".into()
    } else {
        crate::brokers::lower_status(&s)
    }
}

/// Whether a pending-book row is cancellable (web `cancel_all_orders_api`).
pub fn is_cancellable(order: &Value) -> bool {
    const VALID: &[&str] = &[
        "OPEN",
        "PENDING",
        "TRIGGER PENDING",
        "NEW",
        "RECEIVED",
        "PLACED",
        "VALIDATED",
        "PENDING_0",
        "PENDING_1",
        "PENDING_2",
        "ACCEPTED",
    ];
    let status = text_any(order, &["status", "order_status"]).to_ascii_uppercase();
    VALID.contains(&status.as_str())
        || status.contains("PEND")
        || status.contains("OPEN")
        || status.contains("NEW")
        || text(order, "mode").eq_ignore_ascii_case("NEW")
}

/// The id a cancel uses (web tries these fields in order).
pub fn order_id_of(order: &Value) -> String {
    text_any(
        order,
        &[
            "oms_order_id",
            "order_id",
            "id",
            "orderId",
            "nnf_id",
            "exchangeOrderId",
        ],
    )
}

// ---------------------------------------------------------------------------
// Order payloads
// ---------------------------------------------------------------------------

/// Instrument token as the integer Pocketful expects (string when not
/// numeric, so the broker reports the problem rather than a silent 0).
fn token_value(token: &str) -> Value {
    match token.trim().parse::<i64>() {
        Ok(t) => json!(t),
        Err(_) => json!(token.trim()),
    }
}

/// web `transform_data`. `price` and `order_type` are the final values (a
/// MARKET order already converted to a protected LIMIT by the caller).
pub fn place_payload(o: &ResolvedOrder, client_id: &str, order_type: &str, price: f64) -> Value {
    json!({
        "exchange": o.exchange.as_str(),
        "instrument_token": token_value(o.token()),
        "client_id": client_id,
        "order_type": order_type,
        "amo": false,
        "price": price,
        "quantity": o.quantity,
        "disclosed_quantity": o.disclosed_quantity,
        "validity": "DAY",
        "product": product(o.product),
        "order_side": o.action.as_str(),
        "device": "WEB",
        "user_order_id": 1,
        "trigger_price": o.trigger_price,
        "execution_type": "REGULAR",
    })
}

/// web `transform_modify_order_data`.
pub fn modify_payload(m: &ResolvedModify, client_id: &str) -> Value {
    json!({
        "exchange": m.exchange.as_str(),
        "instrument_token": token_value(m.token()),
        "client_id": client_id,
        "order_type": order_type(m.pricetype),
        "price": m.price,
        "quantity": m.quantity,
        "disclosed_quantity": m.disclosed_quantity,
        "validity": "DAY",
        "product": product(m.product),
        "order_side": m.action.as_str(),
        "device": "WEB",
        "user_order_id": 1,
        "trigger_price": m.trigger_price,
        "oms_order_id": m.order_id,
        "execution_type": "REGULAR",
    })
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// Broker symbol -> OpenAlgo symbol (web `get_oa_symbol`, original kept
/// when the master has no row).
fn oa_symbol(symbols: &SymbolResolver, brsymbol: &str, exchange: &str) -> String {
    if brsymbol.is_empty() || exchange.is_empty() {
        return brsymbol.to_string();
    }
    symbols.oa_symbol_or_raw(brsymbol, exchange)
}

fn side(v: &Value) -> String {
    text_any(v, &["order_side", "transaction_type"]).to_ascii_uppercase()
}

/// web `transform_order_data` over the merged completed + pending books.
pub fn map_orders(rows: &[Value], symbols: &SymbolResolver) -> Vec<Order> {
    rows.iter()
        .filter(|o| o.is_object())
        .map(|o| {
            let exchange = text(o, "exchange");
            let br = text_any(o, &["trading_symbol", "tradingsymbol"]);
            let status = map_status(&text(o, "order_status"), &text(o, "mode"));
            let average = num_any(o, &["average_price"])
                .filter(|v| *v != 0.0)
                .or_else(|| num_any(o, &["average_trade_price"]))
                .unwrap_or(0.0);
            let reason = text_any(o, &["rejection_reason", "reject_reason", "reason"]);
            let exchange_order_id = text_any(o, &["exchange_order_id", "exchangeOrderId"]);
            Order {
                order_tag: None,
                order_id: text_any(o, &["oms_order_id", "order_id"]),
                exchange_order_id: (!exchange_order_id.is_empty()).then_some(exchange_order_id),
                symbol: oa_symbol(symbols, &br, &exchange),
                exchange,
                side: side(o),
                quantity: i32_of(int(o, "quantity")),
                filled_quantity: i32_of(int(o, "filled_quantity")),
                pending_quantity: i32_of(int(o, "remaining_quantity")),
                price: num(o, "price"),
                trigger_price: num(o, "trigger_price"),
                average_price: average,
                order_type: oa_pricetype(&text(o, "order_type")),
                product: oa_product(&text(o, "product")),
                rejection_reason: (status == "rejected" && !reason.is_empty()).then_some(reason),
                status,
                validity: {
                    let v = text(o, "validity");
                    if v.is_empty() {
                        "DAY".into()
                    } else {
                        v
                    }
                },
                order_timestamp: text_any(o, &["order_entry_time", "order_timestamp"]),
                exchange_timestamp: None,
            }
        })
        .collect()
}

/// web `map_trade_data` + `transform_tradebook_data`.
pub fn map_trades(rows: &[Value], symbols: &SymbolResolver) -> Vec<Trade> {
    rows.iter()
        .filter(|t| t.is_object())
        .map(|t| {
            let exchange = text(t, "exchange");
            let br = text_any(t, &["trading_symbol", "tradingsymbol"]);
            let quantity = num_any(t, &["fill_quantity", "trade_quantity"]).unwrap_or(0.0) as i64;
            let average = num_any(t, &["avg_price", "trade_price"]).unwrap_or(0.0);
            Trade {
                order_tag: None,
                order_id: text_any(t, &["order_id", "oms_order_id"]),
                trade_id: text_any(t, &["trade_id", "trade_number"]),
                symbol: oa_symbol(symbols, &br, &exchange),
                exchange,
                product: oa_product(&text(t, "product")),
                side: side(t),
                quantity: i32_of(quantity),
                average_price: average,
                trade_value: if quantity > 0 && average > 0.0 {
                    quantity as f64 * average
                } else {
                    0.0
                },
                timestamp: text_any(t, &["fill_timestamp", "trade_time"]),
            }
        })
        .collect()
}

/// Net quantity of a position row (`net_quantity` first, like
/// `map_position_data`; the smart-order reader takes `quantity` first).
pub fn position_qty(p: &Value) -> i64 {
    num_any(p, &["net_quantity", "quantity"]).unwrap_or(0.0) as i64
}

/// web `map_position_data` + `transform_positions_data`.
pub fn map_positions(rows: &[Value], symbols: &SymbolResolver) -> Vec<Position> {
    rows.iter()
        .filter(|p| p.is_object())
        .map(|p| {
            let exchange = text(p, "exchange");
            let br = text_any(p, &["trading_symbol", "tradingsymbol"]);
            let qty = position_qty(p);
            let buy_avg = num(p, "average_buy_price");
            let sell_avg = num(p, "average_sell_price");
            let average = if buy_avg > 0.0 {
                buy_avg
            } else if sell_avg > 0.0 {
                sell_avg
            } else {
                num(p, "average_price")
            };
            let ltp = num_any(p, &["ltp", "last_price"]).unwrap_or(0.0);
            let pnl = if p.get("ltp").is_some() && qty > 0 && p.get("average_buy_price").is_some() {
                (ltp - buy_avg) * qty as f64
            } else if p.get("ltp").is_some() && qty < 0 && p.get("average_sell_price").is_some() {
                (sell_avg - ltp) * qty.unsigned_abs() as f64
            } else {
                num(p, "pnl")
            };
            let buy_qty = int(p, "buy_quantity");
            let sell_qty = int(p, "sell_quantity");
            Position {
                symbol: oa_symbol(symbols, &br, &exchange),
                exchange,
                product: oa_product(&text(p, "product")),
                quantity: i32_of(qty),
                overnight_quantity: i32_of(int(p, "previous_quantity")),
                average_price: round2(average),
                ltp,
                pnl,
                realized_pnl: num_any(p, &["realized_pnl", "realised_pnl", "realized_mtm"])
                    .unwrap_or(0.0),
                unrealized_pnl: num_any(p, &["unrealized_pnl", "unrealised_pnl", "unrealized_mtm"])
                    .unwrap_or(pnl),
                buy_quantity: i32_of(buy_qty),
                buy_value: buy_qty as f64 * buy_avg,
                sell_quantity: i32_of(sell_qty),
                sell_value: sell_qty as f64 * sell_avg,
            }
        })
        .collect()
}

/// web `transform_holdings_data`.
pub fn map_holdings(rows: &[Value], symbols: &SymbolResolver) -> Vec<Holding> {
    rows.iter()
        .filter(|h| h.is_object())
        .filter_map(|h| {
            let mut br = text_any(h, &["tradingsymbol", "trading_symbol", "symbol"]);
            if br.is_empty() {
                let d = &h["instrument_details"];
                br = text_any(d, &["trading_symbol", "symbol"]);
            }
            if br.is_empty() {
                return None;
            }
            let exchange = {
                let e = text(h, "exchange");
                if e.is_empty() {
                    "NSE".to_string()
                } else {
                    e
                }
            };
            // The master keeps the `-EQ` series on NSE brsymbols; the web
            // strips it and looks the clean name up (falling back to it).
            let symbol = symbols
                .oa_symbol(&br, &exchange)
                .or_else(|| {
                    let clean = br.strip_suffix("-EQ").unwrap_or(&br);
                    symbols.oa_symbol(clean, &exchange)
                })
                .unwrap_or_else(|| br.strip_suffix("-EQ").unwrap_or(&br).to_string());
            let quantity = num_any(h, &["quantity", "free_quantity", "qty"]).unwrap_or(0.0) as i64;
            let average =
                num_any(h, &["average_price", "buy_avg", "avg_price", "buy_price"]).unwrap_or(0.0);
            let ltp =
                num_any(h, &["last_price", "ltp", "current_price", "market_price"]).unwrap_or(0.0);
            let computed = if quantity > 0 && average > 0.0 {
                (ltp - average) * quantity as f64
            } else {
                0.0
            };
            let pnl = num_any(h, &["pnl"]).unwrap_or(computed);
            let pct_computed = if average > 0.0 {
                (ltp - average) / average * 100.0
            } else {
                0.0
            };
            let pnl_pct = num_any(h, &["pnl_percent", "pnl_percentage"]).unwrap_or(pct_computed);
            let isin = text(h, "isin");
            Some(Holding {
                symbol,
                exchange,
                product: "CNC".into(),
                isin: (!isin.is_empty()).then_some(isin),
                quantity: i32_of(quantity),
                t1_quantity: i32_of(int(h, "t1_quantity")),
                average_price: round2(average),
                ltp: round2(ltp),
                close_price: num_any(h, &["close_price", "previous_close"]).unwrap_or(0.0),
                pnl: round2(pnl),
                pnl_percentage: round2(pnl_pct),
                current_value: round2(ltp * quantity as f64),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Funds
// ---------------------------------------------------------------------------

/// web `get_margin_data`: `data.values` is a list of `[label, value]`.
pub fn map_funds(body: &Value) -> Funds {
    let values = body["data"]["values"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let lookup = |label: &str| -> f64 {
        values
            .iter()
            .find(|pair| pair.get(0).and_then(Value::as_str) == Some(label))
            .and_then(|pair| pair.get(1).and_then(as_f64))
            .unwrap_or(0.0)
    };
    let available = lookup("Available Margin");
    let used = lookup("Margin Used");
    Funds {
        available_cash: round2(available),
        used_margin: round2(used),
        collateral: round2(lookup("Total Pledge Collateral")),
        m2m_unrealized: round2(lookup("unrealized_mtm")),
        m2m_realized: round2(lookup("realized_mtm")),
        utilised_debits: round2(used),
        ..Default::default()
    }
}
