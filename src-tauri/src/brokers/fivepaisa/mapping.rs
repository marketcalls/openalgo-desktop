//! 5paisa <-> OpenAlgo maps and book normalisers (web
//! `mapping/transform_data.py`, `mapping/order_data.py`).

use crate::brokers::common::mapping::{Action, Exchange, Product};
use crate::brokers::common::mpp::{instrument_type_from_symbol, mpp_percentage, py_round};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Lenient field access
// ---------------------------------------------------------------------------

pub fn num(v: &Value, k: &str) -> f64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub fn int(v: &Value, k: &str) -> i64 {
    num(v, k) as i64
}

/// String, number (as text) or empty.
pub fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|i| i.to_string())
            .unwrap_or_else(|| n.to_string()),
        _ => String::new(),
    }
}

/// The array at `body.<key>`, empty when null or absent.
pub fn body_rows(v: &Value, key: &str) -> Vec<Value> {
    v.get("body")
        .and_then(|b| b.get(key))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Enum maps
// ---------------------------------------------------------------------------

/// web `map_action`.
pub fn action_code(a: Action) -> &'static str {
    match a {
        Action::Buy => "B",
        Action::Sell => "S",
    }
}

/// web `map_exchange`.
pub fn exch_code(exchange: &str) -> &'static str {
    match exchange {
        "NSE" | "NFO" | "CDS" | "NSE_INDEX" => "N",
        "BSE" | "BFO" | "BCD" | "BSE_INDEX" => "B",
        "MCX" => "M",
        _ => "N",
    }
}

/// web `map_exchange_type`.
pub fn exch_type(exchange: &str) -> &'static str {
    match exchange {
        "NSE" | "BSE" | "NSE_INDEX" | "BSE_INDEX" => "C",
        "NFO" | "BFO" | "MCX" => "D",
        "CDS" | "BCD" => "U",
        _ => "C",
    }
}

/// web `reverse_map_exchange`.
pub fn reverse_exchange(exch: &str, exch_type: &str) -> Option<&'static str> {
    Some(match (exch, exch_type) {
        ("N", "C") => "NSE",
        ("B", "C") => "BSE",
        ("N", "D") => "NFO",
        ("B", "D") => "BFO",
        ("N", "U") => "CDS",
        ("B", "U") => "BCD",
        ("M", "D") => "MCX",
        _ => return None,
    })
}

/// web `map_product_type`: CNC/NRML -> D, MIS -> I.
pub fn product_code(p: Product) -> &'static str {
    match p {
        Product::Mis => "I",
        Product::Cnc | Product::Nrml => "D",
    }
}

/// web `reverse_map_product_type` plus the book rule: `D` is CNC on cash,
/// NRML elsewhere; `I` is MIS. Unknown codes pass through.
pub fn reverse_product(code: &str, exchange: &str) -> String {
    match code {
        "I" => "MIS".into(),
        "D" if matches!(exchange, "NSE" | "BSE") => "CNC".into(),
        "D" => "NRML".into(),
        other => other.to_string(),
    }
}

/// web `normalize_order_status`.
pub fn normalize_status(status: &str) -> String {
    let raw = status.trim().to_ascii_lowercase();
    let mapped = match raw.as_str() {
        "fully executed" => "complete",
        "pending" | "open" | "modified" | "placed" | "ah placed" | "ah modified" | "xmitted" => {
            "open"
        }
        "cancelled" | "canceled" | "ah cancelled" => "cancelled",
        "rejected by 5p" | "rejected by exch" => "rejected",
        _ if raw.contains("rejected") => "rejected",
        _ if raw.contains("cancel") => "cancelled",
        _ => return raw,
    };
    mapped.to_string()
}

/// web orderbook pricetype from `AtMarket` and `SLTriggerRate`.
pub fn book_pricetype(at_market: &str, trigger: f64) -> &'static str {
    match (at_market, trigger > 0.0) {
        ("Y", false) => "MARKET",
        ("N", false) => "LIMIT",
        ("Y", true) => "SL-M",
        ("N", true) => "SL",
        _ => "LIMIT",
    }
}

/// `/Date(1718000000000+0530)/` -> `2024-06-10 11:43:20` (UTC shifted by
/// the offset, like the web's `convert_date_string`). A value without an
/// offset is read as UTC; anything else gives an empty string.
pub fn ms_date(s: &str) -> String {
    let Some(inner) = s
        .trim()
        .strip_prefix("/Date(")
        .and_then(|r| r.strip_suffix(")/"))
    else {
        return String::new();
    };
    let split = inner
        .char_indices()
        .skip(1)
        .find(|(_, c)| *c == '+' || *c == '-')
        .map(|(i, _)| i);
    let (ms, off) = match split {
        Some(i) => (&inner[..i], &inner[i..]),
        None => (inner, ""),
    };
    let Ok(ms) = ms.parse::<i64>() else {
        return String::new();
    };
    let mut offset_secs = 0i64;
    if off.len() == 5 {
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let h: i64 = off[1..3].parse().unwrap_or(0);
        let m: i64 = off[3..5].parse().unwrap_or(0);
        offset_secs = sign * (h * 3600 + m * 60);
    }
    chrono::DateTime::from_timestamp(ms.div_euclid(1000) + offset_secs, 0)
        .map(|d| d.naive_utc().format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Epoch ms inside `/Date(…)/` (feed `TickDt`), 0 when absent.
pub fn ms_date_epoch(s: &str) -> i64 {
    s.trim()
        .strip_prefix("/Date(")
        .map(|r| {
            r.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
        })
        .and_then(|d| d.parse().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Order bodies
// ---------------------------------------------------------------------------

fn tick_decimals(tick: f64) -> i32 {
    let s = format!("{}", tick);
    s.split_once('.').map(|(_, f)| f.len() as i32).unwrap_or(0)
}

fn snap_to_tick(value: f64, tick: f64, floor: bool) -> f64 {
    let ratio = py_round(value / tick, 6);
    let k = if floor { ratio.floor() } else { ratio.ceil() };
    py_round(k * tick, tick_decimals(tick))
}

/// web `_slm_protected_price`: a stop-limit just beyond the trigger (SELL
/// below, BUY above), at least one tick away, tick-aligned. Refused when
/// the tick size is unknown.
pub fn slm_protected_price(symbol: &str, action: Action, trigger: f64, tick: f64) -> Result<f64> {
    if !tick.is_finite() || tick <= 0.0 {
        return Err(AppError::Validation(format!(
            "The tick size of {} is not known, so the SL-M order cannot be priced. Download the master contract again.",
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
                    "The SL-M trigger {} for {} is too low to place a protected stop order.",
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

/// web `transform_data` with the price already decided (MPP or SL-M).
pub fn place_body(o: &ResolvedOrder, price: f64) -> Value {
    let ex = o.exchange.as_str();
    json!({
        "OrderType": action_code(o.action),
        "Exchange": exch_code(ex),
        "ExchangeType": exch_type(ex),
        "ScripCode": o.token(),
        "Price": price,
        "Qty": o.quantity,
        "StopLossPrice": o.trigger_price,
        "DisQty": o.disclosed_quantity,
        "IsIntraday": o.product == Product::Mis,
        "AHPlaced": "N",
        "RemoteOrderID": "OpenAlgo",
    })
}

/// web `transform_modify_order_data`.
pub fn modify_body(m: &ResolvedModify, exch_order_id: &str) -> Value {
    json!({
        "ExchOrderID": exch_order_id,
        "Price": m.price,
        "Qty": m.quantity,
        "StopLossPrice": m.trigger_price,
        "DisQty": m.disclosed_quantity,
    })
}

/// The order id of an accepted placement: `head.statusDescription ==
/// "Success"`, `body.Status == 0` and a non-zero `BrokerOrderID`; otherwise
/// the broker's reason.
pub fn placed_order_id(v: &Value) -> std::result::Result<String, String> {
    let body = v.get("body").cloned().unwrap_or(Value::Null);
    let id = text(&body, "BrokerOrderID");
    let status_ok = match body.get("Status") {
        Some(Value::Number(n)) => n.as_i64() == Some(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok() == Some(0),
        _ => false,
    };
    if super::head_success(v) && status_ok && !id.is_empty() && id != "0" {
        Ok(id)
    } else {
        let m = text(&body, "Message");
        Err(if !m.is_empty() {
            m
        } else {
            let h = v
                .get("head")
                .map(|h| text(h, "statusDescription"))
                .unwrap_or_default();
            if h.is_empty() || h == "Success" {
                "Order rejected by 5Paisa".into()
            } else {
                h
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// OpenAlgo symbol and exchange of a book row from its scrip code.
fn resolve(symbols: &SymbolResolver, row: &Value) -> (String, String) {
    let exch = text(row, "Exch");
    let et = text(row, "ExchType");
    let exchange = reverse_exchange(&exch, &et).unwrap_or("").to_string();
    let token = text(row, "ScripCode");
    match symbols.by_token(&exchange, &token) {
        Some(r) => (r.symbol, exchange),
        None => {
            let name = text(row, "ScripName");
            (name, exchange)
        }
    }
}

fn side(code: &str) -> String {
    match code {
        "B" => "BUY".into(),
        "S" => "SELL".into(),
        other => other.to_string(),
    }
}

/// web `map_order_data` + `transform_order_data`.
pub fn to_order(symbols: &SymbolResolver, row: &Value) -> Order {
    let (symbol, exchange) = resolve(symbols, row);
    let trigger = num(row, "SLTriggerRate");
    let quantity = int(row, "Qty") as i32;
    let filled = int(row, "TradedQty") as i32;
    let pending = if row.get("PendingQty").is_some() {
        int(row, "PendingQty") as i32
    } else {
        (quantity - filled).max(0)
    };
    let exch_id = text(row, "ExchOrderID");
    let reason = text(row, "Reason");
    Order {
        order_id: text(row, "BrokerOrderId"),
        exchange_order_id: (!exch_id.is_empty() && exch_id != "0").then_some(exch_id),
        symbol,
        product: reverse_product(&text(row, "DelvIntra"), &exchange),
        exchange,
        side: side(&text(row, "BuySell")),
        quantity,
        filled_quantity: filled,
        pending_quantity: pending,
        price: num(row, "Rate"),
        trigger_price: trigger,
        average_price: num(row, "AveragePrice"),
        order_type: book_pricetype(&text(row, "AtMarket"), trigger).to_string(),
        status: normalize_status(&text(row, "OrderStatus")),
        validity: "DAY".into(),
        order_timestamp: ms_date(&text(row, "BrokerOrderTime")),
        exchange_timestamp: None,
        rejection_reason: (!reason.is_empty()).then_some(reason),
    }
}

/// web `map_trade_data` + `transform_tradebook_data` (the order id is the
/// exchange order id, as on the web).
pub fn to_trade(symbols: &SymbolResolver, row: &Value) -> Trade {
    let (symbol, exchange) = resolve(symbols, row);
    let qty = num(row, "Qty");
    let rate = num(row, "Rate");
    Trade {
        order_id: text(row, "ExchOrderID"),
        trade_id: text(row, "ExchangeTradeID"),
        symbol,
        product: reverse_product(&text(row, "DelvIntra"), &exchange),
        exchange,
        side: side(&text(row, "BuySell")),
        quantity: qty as i32,
        average_price: rate,
        trade_value: py_round(qty * rate, 2),
        timestamp: ms_date(&text(row, "ExchangeTradeTime")),
    }
}

/// web `map_position_data` + `transform_positions_data`.
pub fn to_position(symbols: &SymbolResolver, row: &Value) -> Position {
    let (symbol, exchange) = resolve(symbols, row);
    let net = int(row, "NetQty");
    let avg = if net > 0 {
        num(row, "BuyAvgRate")
    } else {
        num(row, "SellAvgRate")
    };
    let mtom = num(row, "MTOM");
    let booked = num(row, "BookedPL");
    Position {
        symbol,
        product: reverse_product(&text(row, "OrderFor"), &exchange),
        exchange,
        quantity: net as i32,
        overnight_quantity: int(row, "BodQty") as i32,
        average_price: avg,
        ltp: num(row, "LTP"),
        pnl: py_round(mtom + booked, 2),
        realized_pnl: booked,
        unrealized_pnl: mtom,
        buy_quantity: int(row, "BuyQty") as i32,
        buy_value: num(row, "BuyValue"),
        sell_quantity: int(row, "SellQty") as i32,
        sell_value: num(row, "SellValue"),
    }
}

/// web `map_portfolio_data` + `transform_holdings_data`. The symbol comes
/// from the master by `NseCode` / `BseCode` when present, else `Symbol`.
pub fn to_holding(symbols: &SymbolResolver, row: &Value) -> Holding {
    let exchange = match text(row, "Exch").as_str() {
        "B" => "BSE".to_string(),
        "N" => "NSE".to_string(),
        other => other.to_string(),
    };
    let code_key = if exchange == "BSE" {
        "BseCode"
    } else {
        "NseCode"
    };
    let code = text(row, code_key);
    let symbol = symbols
        .by_token(&exchange, &code)
        .filter(|_| !code.is_empty() && code != "0")
        .map(|r| r.symbol)
        .unwrap_or_else(|| text(row, "Symbol"));
    let qty = num(row, "Quantity");
    let avg = num(row, "AvgRate");
    let ltp = num(row, "CurrentPrice");
    let buy = avg * qty;
    let value = ltp * qty;
    let pnl = value - buy;
    Holding {
        symbol,
        exchange,
        product: "CNC".into(),
        isin: None,
        quantity: qty as i32,
        t1_quantity: 0,
        average_price: avg,
        ltp,
        close_price: 0.0,
        pnl: py_round(pnl, 2),
        pnl_percentage: if buy != 0.0 {
            py_round(pnl / buy * 100.0, 2)
        } else {
            0.0
        },
        current_value: value,
    }
}

/// Exchange to look an instrument up under: exact index names on NSE/BSE
/// move to the index exchange (web `normalize_exchange_for_query`).
pub fn query_exchange(symbol: &str, exchange: &str) -> String {
    const INDEX: &[&str] = &[
        "NIFTY",
        "BANKNIFTY",
        "FINNIFTY",
        "MIDCPNIFTY",
        "NIFTYNXT50",
        "SENSEX",
        "BANKEX",
        "SENSEX50",
        "INDIAVIX",
    ];
    if INDEX.contains(&symbol.to_ascii_uppercase().as_str()) {
        match exchange {
            "NSE" => return Exchange::NseIndex.as_str().into(),
            "BSE" => return Exchange::BseIndex.as_str().into(),
            _ => {}
        }
    }
    exchange.to_string()
}

/// A not-found error for a symbol lookup.
pub fn unknown_symbol(symbol: &str, exchange: &str) -> AppError {
    AppError::Validation(format!(
        "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
        symbol, exchange
    ))
}
