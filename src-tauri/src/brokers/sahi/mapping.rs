//! OpenAlgo order fields to Sahi's and Sahi's book rows back (web
//! `mapping/transform_data.py`, `mapping/order_data.py`).
//!
//! Wire values (docs.sahi.com, Conventions, "Enum values"): `txn_type`
//! B/S, `order_type` LMT/MKT/SL-LMT/SL-MKT, `product` C (delivery), I
//! (intraday), M (carry-forward F&O), F (MTF), `validity` DAY/IOC. Prices
//! travel as integer paise. MKT and SL-MKT are sent natively: Sahi converts
//! them once into protected limit orders, so no emulation is needed.

use crate::brokers::common::book_read::whole;
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{FixedOffset, TimeZone};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Rupees to integer paise, rounding half up (web `to_paise`).
pub fn to_paise(rupees: f64) -> i64 {
    if !rupees.is_finite() {
        return 0;
    }
    // Through the shortest decimal text, as the web's Decimal(str(value)),
    // so 2499.55 is 249955 and not 249954.99...
    Decimal::from_str_exact(&rupees.to_string())
        .or_else(|_| Decimal::from_scientific(&rupees.to_string()))
        .ok()
        .and_then(|d| {
            (d * Decimal::from(100))
                .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
                .to_i64()
        })
        .unwrap_or(0)
}

/// A number as Sahi sends one (integer or text); 0 when absent or null.
pub fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Integer paise to rupees, rounded to 2 places (web `from_paise`).
pub fn from_paise(v: Option<&Value>) -> f64 {
    num(v).round() / 100.0
}

pub fn int(v: Option<&Value>) -> i64 {
    v.and_then(whole).unwrap_or(0)
}

fn int32(v: Option<&Value>) -> i32 {
    i32::try_from(int(v)).unwrap_or(0)
}

fn s(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(x)) => x.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

pub fn product_code(p: Product) -> &'static str {
    match p {
        Product::Cnc => "C",
        Product::Mis => "I",
        Product::Nrml => "M",
    }
}

/// Sahi product to OpenAlgo; MTF (`F`) has no OpenAlgo product and is shown
/// as `MTF`, as on the web.
pub fn product_name(code: &str) -> String {
    match code {
        "C" => "CNC",
        "I" => "MIS",
        "M" => "NRML",
        "F" => "MTF",
        other => other,
    }
    .to_string()
}

pub fn order_type_code(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MKT",
        PriceType::Limit => "LMT",
        PriceType::Sl => "SL-LMT",
        PriceType::SlM => "SL-MKT",
    }
}

pub fn order_type_name(code: &str) -> String {
    match code {
        "MKT" => "MARKET",
        "LMT" => "LIMIT",
        "SL-LMT" => "SL",
        "SL-MKT" => "SL-M",
        other => other,
    }
    .to_string()
}

fn action_code(a: Action) -> &'static str {
    match a {
        Action::Buy => "B",
        Action::Sell => "S",
    }
}

fn action_name(code: &str) -> String {
    match code {
        "B" => "BUY",
        "S" => "SELL",
        other => other,
    }
    .to_string()
}

/// OpenAlgo exchange to Sahi `exchange_segment`; indices cannot be traded.
pub fn segment(exchange: Exchange) -> Result<&'static str> {
    match exchange {
        Exchange::Nse => Ok("NSE"),
        Exchange::Bse => Ok("BSE"),
        Exchange::Nfo => Ok("NFO"),
        Exchange::Bfo => Ok("BFO"),
        other => Err(AppError::Validation(format!(
            "Sahi does not take orders on {}.",
            other
        ))),
    }
}

/// Whether Sahi takes `product` on `exchange` (docs.sahi.com, Conventions,
/// "Product"): delivery on NSE and BSE, carry-forward on NFO and BFO,
/// intraday on all four. Any other pair Sahi refuses with 400, so it is
/// refused here first with the product to use.
pub fn product_on_segment(product: Product, exchange: Exchange) -> Result<()> {
    let ok = match product {
        Product::Cnc => matches!(exchange, Exchange::Nse | Exchange::Bse),
        Product::Nrml => matches!(exchange, Exchange::Nfo | Exchange::Bfo),
        Product::Mis => true,
    };
    if ok {
        Ok(())
    } else {
        Err(AppError::Validation(format!(
            "Sahi does not take {} orders on {}. Use CNC or MIS on NSE and BSE, and NRML or MIS on NFO and BFO.",
            product, exchange
        )))
    }
}

/// Sahi order status to OpenAlgo's (web `ORDER_STATUS_MAP`). PartialFilled
/// is terminal (the remainder was cancelled or an IOC expired), so it reads
/// as cancelled; a stop order waiting for its trigger is reported as Open.
/// Sahi's docs also name `AwaitingTrigger` (an order not yet at the
/// exchange) outside their status table; it reads as OpenAlgo's `trigger
/// pending` rather than an unknown word (the web lower-cases it).
pub fn order_status(status: &str) -> String {
    match status {
        "Open" | "OpenPartial" => "open".to_string(),
        "AwaitingTrigger" => "trigger pending".to_string(),
        "PartialFilled" | "Cancelled" => "cancelled".to_string(),
        "Completed" => "complete".to_string(),
        "Rejected" => "rejected".to_string(),
        "" => "open".to_string(),
        other => other.to_lowercase(),
    }
}

/// Whether Sahi will still cancel or modify an order in this status.
pub fn is_working(status: &str) -> bool {
    matches!(status, "Open" | "OpenPartial")
}

fn price_fields(
    body: &mut Map<String, Value>,
    pricetype: PriceType,
    price: f64,
    trigger_price: f64,
) {
    // MKT and SL-MKT take price 0; Sahi prices the protected limit itself.
    let market = matches!(pricetype, PriceType::Market | PriceType::SlM);
    body.insert(
        "price".into(),
        json!(if market { 0 } else { to_paise(price) }),
    );
    if matches!(pricetype, PriceType::Sl | PriceType::SlM) {
        body.insert("trigger_price".into(), json!(to_paise(trigger_price)));
    }
}

/// The `POST /v1/orders` body (web `transform_data`); margin calls take the
/// same body.
pub fn place_body(o: &ResolvedOrder) -> Result<Value> {
    let mut body = Map::new();
    body.insert("exchange_segment".into(), json!(segment(o.exchange)?));
    product_on_segment(o.product, o.exchange)?;
    body.insert("symbol".into(), json!(o.brsymbol()));
    body.insert("txn_type".into(), json!(action_code(o.action)));
    body.insert("qty".into(), json!(o.quantity));
    body.insert("order_type".into(), json!(order_type_code(o.pricetype)));
    body.insert("product".into(), json!(product_code(o.product)));
    body.insert("validity".into(), json!("DAY"));
    price_fields(&mut body, o.pricetype, o.price, o.trigger_price);
    if o.disclosed_quantity > 0 {
        body.insert("disclosed_qty".into(), json!(o.disclosed_quantity));
    }
    Ok(Value::Object(body))
}

/// The `PUT /v1/orders/{id}` body (web `transform_modify_order_data`): the
/// full set the trader submitted.
pub fn modify_body(m: &ResolvedModify) -> Value {
    let mut body = Map::new();
    body.insert("qty".into(), json!(m.quantity));
    body.insert("order_type".into(), json!(order_type_code(m.pricetype)));
    body.insert("validity".into(), json!("DAY"));
    price_fields(&mut body, m.pricetype, m.price, m.trigger_price);
    body.insert("disclosed_qty".into(), json!(m.disclosed_quantity));
    Value::Object(body)
}

/// Epoch milliseconds to IST `YYYY-MM-DD HH:MM:SS`; empty when unset.
pub fn ist_timestamp(v: Option<&Value>) -> String {
    let ms = int(v);
    if ms <= 0 {
        return String::new();
    }
    let ist = FixedOffset::east_opt(crate::clock::IST_OFFSET_SECS as i32);
    match (ist, chrono::DateTime::from_timestamp_millis(ms)) {
        (Some(tz), Some(t)) => tz
            .from_utc_datetime(&t.naive_utc())
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        _ => String::new(),
    }
}

/// OpenAlgo symbol of a Sahi `(symbol, exchange_segment)`, else Sahi's.
fn oa_symbol(symbols: &SymbolResolver, row: &Value) -> (String, String) {
    let segment = s(row, "exchange_segment");
    let brsymbol = s(row, "symbol");
    (symbols.oa_symbol_or_raw(&brsymbol, &segment), segment)
}

pub fn map_order(row: &Value, symbols: &SymbolResolver) -> Order {
    let (symbol, exchange) = oa_symbol(symbols, row);
    let quantity = int32(row.get("qty"));
    let filled = int32(row.get("filled_qty"));
    let raw_status = s(row, "status");
    let rejection = s(row, "rejection_reason");
    Order {
        order_id: s(row, "order_id"),
        exchange_order_id: Some(s(row, "exchange_order_id")).filter(|x| !x.is_empty()),
        symbol,
        exchange,
        side: action_name(&s(row, "txn_type")),
        quantity,
        filled_quantity: filled,
        pending_quantity: if is_working(&raw_status) {
            (quantity - filled).max(0)
        } else {
            0
        },
        price: from_paise(row.get("price")),
        trigger_price: from_paise(row.get("trigger_price")),
        average_price: from_paise(row.get("avg_price")),
        order_type: order_type_name(&s(row, "order_type")),
        product: product_name(&s(row, "product")),
        status: order_status(&raw_status),
        validity: s(row, "validity"),
        order_timestamp: ist_timestamp(row.get("created_at")),
        exchange_timestamp: Some(ist_timestamp(row.get("placed_at"))).filter(|x| !x.is_empty()),
        rejection_reason: Some(rejection).filter(|x| !x.is_empty()),
        order_tag: None,
    }
}

pub fn map_trade(row: &Value, symbols: &SymbolResolver) -> Trade {
    let (symbol, exchange) = oa_symbol(symbols, row);
    let quantity = int32(row.get("qty"));
    let price = from_paise(row.get("price"));
    Trade {
        order_id: s(row, "order_id"),
        trade_id: s(row, "exchange_trade_id"),
        symbol,
        exchange,
        product: product_name(&s(row, "product")),
        side: action_name(&s(row, "txn_type")),
        quantity,
        average_price: price,
        trade_value: round2(f64::from(quantity) * price),
        timestamp: ist_timestamp(row.get("traded_at")),
        order_tag: None,
    }
}

pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// A position row; `ltp` is the last price read for its `instrument_id`
/// (0 when unknown). Sahi reports realised P&L only, so the open part is
/// `(ltp - avg) * qty`, left out rather than guessed without an LTP.
pub fn map_position(row: &Value, ltp: f64, symbols: &SymbolResolver) -> Position {
    let (symbol, exchange) = oa_symbol(symbols, row);
    let net = int32(row.get("net_qty"));
    let avg = from_paise(row.get("net_avg_price"));
    let realized = from_paise(row.get("realised_pnl"));
    let unrealized = if net != 0 && ltp > 0.0 {
        (ltp - avg) * f64::from(net)
    } else {
        0.0
    };
    Position {
        symbol,
        exchange,
        product: product_name(&s(row, "product")),
        quantity: net,
        average_price_basis: None,
        overnight_quantity: int32(row.get("cf_buy_qty")) - int32(row.get("cf_sell_qty")),
        average_price: if net != 0 { avg } else { 0.0 },
        ltp,
        pnl: round2(realized + unrealized),
        realized_pnl: realized,
        unrealized_pnl: round2(unrealized),
        buy_quantity: int32(row.get("total_buy_qty")),
        buy_value: from_paise(row.get("total_buy_amt")),
        sell_quantity: int32(row.get("total_sell_qty")),
        sell_value: from_paise(row.get("total_sell_amt")),
    }
}

/// A holding with settled or T1 quantity, valued at its NSE listing (else
/// BSE) and that listing's LTP. Shares bought for delivery today have no
/// holding quantity yet; they are in the position book (web).
pub fn map_holding(
    row: &Value,
    ltps: &HashMap<String, f64>,
    symbols: &SymbolResolver,
) -> Option<Holding> {
    let quantity = int32(row.get("holding_qty"));
    if quantity <= 0 {
        return None;
    }
    let ids: Vec<String> = row
        .get("instrument_ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| int(Some(v)).to_string()).collect())
        .unwrap_or_default();
    let listing = ["NSE", "BSE"].iter().find_map(|ex| {
        ids.iter()
            .find_map(|id| symbols.by_token(ex, id).map(|r| (r.symbol, *ex, id)))
    });
    let Some((symbol, exchange, id)) = listing else {
        tracing::info!("A Sahi holding is not in the master contract; skipped");
        return None;
    };
    let avg = from_paise(row.get("holding_avg_price"));
    let ltp = ltps.get(id).copied().unwrap_or(0.0);
    let q = f64::from(quantity);
    let pnl = if ltp > 0.0 {
        round2((ltp - avg) * q)
    } else {
        0.0
    };
    Some(Holding {
        symbol,
        exchange: exchange.to_string(),
        product: "CNC".to_string(),
        isin: Some(s(row, "isin")).filter(|x| !x.is_empty()),
        quantity,
        t1_quantity: int32(row.get("t1_qty")),
        average_price: avg,
        ltp,
        close_price: 0.0,
        pnl,
        pnl_percentage: if ltp > 0.0 && avg > 0.0 {
            round2((ltp - avg) / avg * 100.0)
        } else {
            0.0
        },
        current_value: round2(ltp * q),
    })
}
