//! Tradejini <-> OpenAlgo mappings (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, the book transforms in `api/order_api.py` and
//! `api/funds.py`).
//!
//! Every Tradejini response is the envelope `{"s": "ok"|"no-data"|"error",
//! "d": ..., "msg": ...}`; `no-data` is an empty, non-error result. Books
//! are requested with `symDetails=true`, which adds a `sym` object whose
//! field names vary (`id|symId`, `exchange|exch`, `symbol|sym`,
//! `tradSymbol|trdSym|dispSymbol|dispSym`); the readers below accept all.

use crate::brokers::common::mapping::{Exchange, PriceType, Product, Validity};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{Funds, Holding, Order, Position, Trade};
use serde_json::{Map, Value};

// ---------------------------------------------------------------------------
// Lenient readers
// ---------------------------------------------------------------------------

/// Number or numeric string -> f64 (0 otherwise), like the web's `float()`
/// guarded by `or 0`.
pub fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub fn f(v: &Value, k: &str) -> f64 {
    num(v.get(k))
}

/// A number as the web's `float()` reads it, for the strict position read
/// (#2116): missing is `default`, a JSON number or numeric text is its
/// value, and null or any other text is `None` (where Python raises).
pub fn num_strict(v: Option<&Value>, default: f64) -> Option<f64> {
    match v {
        None => Some(default),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Integer like Python's `int(float(x))`.
pub fn i(v: &Value, k: &str) -> i64 {
    num(v.get(k)) as i64
}

/// String, number or null -> String.
pub fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// First present, non-empty value among `names` (web `_first`).
fn first(sym: &Value, names: &[&str]) -> String {
    for n in names {
        let x = s(sym, n);
        if !x.is_empty() {
            return x;
        }
    }
    String::new()
}

/// Symbol id, e.g. `EQT_RELIANCE_EQ_NSE` (web `sym_id`).
pub fn sym_id(sym: &Value) -> String {
    first(sym, &["id", "symId"])
}

/// Exchange of the instrument (web `sym_exchange`).
pub fn sym_exchange(sym: &Value) -> String {
    first(sym, &["exchange", "exch"])
}

/// Base symbol, e.g. `RELIANCE` (web `sym_base_symbol`).
pub fn sym_base_symbol(sym: &Value) -> String {
    first(sym, &["symbol", "sym"])
}

/// Exchange trading symbol, e.g. `RELIANCE-EQ` (web `sym_trading_symbol`).
pub fn sym_trading_symbol(sym: &Value) -> String {
    first(sym, &["tradSymbol", "trdSym", "dispSymbol", "dispSym"])
}

fn sym_of(row: &Value) -> Value {
    match row.get("sym") {
        Some(v @ Value::Object(_)) => v.clone(),
        _ => Value::Object(Map::new()),
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// Error text of an envelope: top-level `msg`, else `d.msg`
/// (web `_envelope_error`).
pub fn envelope_error(v: &Value) -> Option<String> {
    let top = s(v, "msg");
    if !top.trim().is_empty() {
        return Some(top.trim().to_string());
    }
    v.get("d")
        .map(|d| s(d, "msg"))
        .filter(|m| !m.trim().is_empty())
        .map(|m| m.trim().to_string())
}

/// The `d` array of a book response (`no-data` and null become empty).
pub fn rows(v: &Value) -> Vec<Value> {
    match v.get("d") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// OpenAlgo -> Tradejini
// ---------------------------------------------------------------------------

/// web `map_order_type`.
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "market",
        PriceType::Limit => "limit",
        PriceType::Sl => "stoplimit",
        PriceType::SlM => "stopmarket",
    }
}

/// web `map_product_type`.
pub fn product(p: Product) -> &'static str {
    match p {
        Product::Cnc => "delivery",
        Product::Nrml => "normal",
        Product::Mis => "intraday",
    }
}

/// web `map_validity` (OpenAlgo only sends DAY / IOC; EOS is BSE-only and
/// GTC is not an OpenAlgo validity).
pub fn validity(v: Validity, _exchange: Exchange) -> &'static str {
    match v {
        Validity::Day => "day",
        Validity::Ioc => "ioc",
    }
}

/// Python `str(float)`: `100.0` -> `100.0`, `100.5` -> `100.5`.
pub fn py_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

// ---------------------------------------------------------------------------
// Tradejini -> OpenAlgo
// ---------------------------------------------------------------------------

/// web `reverse_map_product_type` (None for unknown values).
pub fn reverse_product(p: &str) -> Option<&'static str> {
    match p.trim().to_ascii_lowercase().as_str() {
        "delivery" => Some("CNC"),
        "normal" | "margin" => Some("NRML"),
        "intraday" => Some("MIS"),
        "cover" | "coverorder" => Some("CO"),
        "bracket" | "bracketorder" => Some("BO"),
        _ => None,
    }
}

/// web `reverse_map_order_type` (unknown values uppercased).
pub fn reverse_order_type(t: &str) -> String {
    match t.trim().to_ascii_lowercase().as_str() {
        "market" => "MARKET".into(),
        "limit" => "LIMIT".into(),
        "stoplimit" => "SL".into(),
        "stopmarket" => "SL-M".into(),
        other => other.to_ascii_uppercase(),
    }
}

/// web `map_order_data` status table; unknown statuses pass through
/// lowercased.
pub fn order_status(raw: &str) -> String {
    let r = raw.trim().to_ascii_lowercase();
    match r.as_str() {
        "completed" | "traded" | "filled" | "complete" => "complete".into(),
        "open" | "pending" => "open".into(),
        "trigger pending" => "trigger pending".into(),
        "rejected" => "rejected".into(),
        "cancelled" | "canceled" => "cancelled".into(),
        _ => r,
    }
}

/// Statuses the web's cancel-all treats as cancellable (raw, uppercased:
/// OPEN, TRIGGER PENDING, MODIFIED, PENDING), after `order_status`.
pub fn is_cancellable(status: &str) -> bool {
    matches!(status, "open" | "trigger pending" | "modified" | "pending")
}

/// OpenAlgo symbol for a book row: the master lookup of the symbol id on
/// its exchange, else the broker's own symbol.
fn resolve(symbols: &SymbolResolver, ids: &[&str], exchange: &str) -> Option<String> {
    ids.iter()
        .filter(|id| !id.is_empty())
        .find_map(|id| symbols.oa_symbol(id, exchange))
}

/// One order-book row (web `get_order_book` + `map_order_data` +
/// `transform_order_data`).
pub fn order_row(o: &Value, symbols: &SymbolResolver) -> Order {
    let sym = sym_of(o);
    let exchange = sym_exchange(&sym);
    let id = sym_id(&sym);
    let sym_id_field = s(o, "symId");
    let symbol = resolve(symbols, &[&id, &sym_id_field], &exchange).unwrap_or_else(|| {
        let t = sym_trading_symbol(&sym);
        if t.is_empty() {
            sym_base_symbol(&sym)
        } else {
            t
        }
    });
    let reason = s(o, "reason");
    let validity = s(o, "validity").to_ascii_uppercase();
    Order {
        order_id: s(o, "orderId"),
        exchange_order_id: Some(s(o, "exchOrderId")).filter(|x| !x.is_empty()),
        symbol,
        exchange,
        side: if s(o, "side").eq_ignore_ascii_case("buy") {
            "BUY".into()
        } else {
            "SELL".into()
        },
        quantity: i(o, "qty") as i32,
        filled_quantity: i(o, "fillQty") as i32,
        pending_quantity: i(o, "pendingQty") as i32,
        price: f(o, "limitPrice"),
        trigger_price: f(o, "trigPrice"),
        average_price: f(o, "avgPrice"),
        order_type: reverse_order_type(&s(o, "type")),
        product: reverse_product(&s(o, "product")).unwrap_or("MIS").into(),
        status: order_status(&s(o, "status")),
        validity: if validity.is_empty() {
            "DAY".into()
        } else {
            validity
        },
        order_timestamp: s(o, "orderTime"),
        exchange_timestamp: None,
        rejection_reason: Some(reason).filter(|r| !r.trim().is_empty()),
    }
}

/// One trade-book row (web `get_trade_book`).
pub fn trade_row(t: &Value, symbols: &SymbolResolver) -> Trade {
    let sym = sym_of(t);
    let exchange = sym_exchange(&sym).to_ascii_uppercase();
    let id = sym_id(&sym);
    let id = if id.is_empty() { s(t, "symId") } else { id };
    let symbol = resolve(symbols, &[&id], &exchange).unwrap_or_else(|| {
        let b = sym_base_symbol(&sym);
        if b.is_empty() {
            sym_trading_symbol(&sym)
        } else {
            b
        }
    });
    Trade {
        order_id: s(t, "orderId"),
        trade_id: first(t, &["tradeId", "fillId", "exchTradeId"]),
        symbol,
        exchange,
        product: reverse_product(&s(t, "product")).unwrap_or("NRML").into(),
        side: if s(t, "side").eq_ignore_ascii_case("buy") {
            "BUY".into()
        } else {
            "SELL".into()
        },
        quantity: i(t, "fillQty") as i32,
        average_price: f(t, "fillPrice"),
        trade_value: f(t, "fillValue"),
        timestamp: s(t, "time"),
    }
}

/// One position row (web `get_positions`).
pub fn position_row(p: &Value, symbols: &SymbolResolver) -> Position {
    let sym = sym_of(p);
    let exchange = sym_exchange(&sym);
    let base = sym_base_symbol(&sym);
    let id = sym_id(&sym);
    let pos_id = s(p, "symId");
    let symbol = resolve(symbols, &[&id, &pos_id, &base], &exchange).unwrap_or(base);
    let realized = f(p, "realizedPnl");
    let avg = (f(p, "netAvgPrice") * 100.0).round() / 100.0;
    Position {
        symbol,
        exchange,
        product: reverse_product(&s(p, "product")).unwrap_or("MIS").into(),
        quantity: i(p, "netQty") as i32,
        overnight_quantity: 0,
        average_price: avg,
        ltp: 0.0,
        pnl: realized,
        realized_pnl: realized,
        unrealized_pnl: 0.0,
        buy_quantity: 0,
        buy_value: 0.0,
        sell_quantity: 0,
        sell_value: 0.0,
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// One holding (web `transform_holdings_data` / `_holding_values`). The
/// response has no LTP, so the average price values the holding unless the
/// symbol object carries `lastPrice` / `ltp`. Rows without a symbol are
/// dropped.
pub fn holding_row(h: &Value, symbols: &SymbolResolver) -> Option<Holding> {
    let sym = sym_of(h);
    let trade_symbol = {
        let t = sym_trading_symbol(&sym);
        if t.is_empty() {
            sym_base_symbol(&sym)
        } else {
            t
        }
    };
    if sym.as_object().map(Map::is_empty).unwrap_or(true) || trade_symbol.is_empty() {
        return None;
    }
    let exchange = sym_exchange(&sym);
    let id = sym_id(&sym);
    let id = if id.is_empty() { s(h, "symId") } else { id };
    let symbol = resolve(symbols, &[&id], &exchange).unwrap_or(trade_symbol);
    let quantity = if h.get("qty").is_some() {
        f(h, "qty")
    } else {
        f(h, "saleableQty")
    };
    let avg = f(h, "avgPrice");
    let ltp_raw = {
        let l = first(&sym, &["lastPrice", "ltp"]);
        l.parse::<f64>().unwrap_or(avg)
    };
    let ltp = if ltp_raw == 0.0 { avg } else { ltp_raw };
    let realized = f(h, "realizedPnl");
    let inv = quantity * avg;
    let cur = if ltp > 0.0 { quantity * ltp } else { inv };
    let pnl = (cur - inv) + realized;
    let pct = if inv > 0.0 { pnl / inv * 100.0 } else { 0.0 };
    let product = if matches!(
        s(h, "product").to_ascii_uppercase().as_str(),
        "MIS" | "INTRADAY"
    ) {
        "MIS"
    } else {
        "CNC"
    };
    Some(Holding {
        symbol: symbol.trim().to_string(),
        exchange: if exchange.is_empty() {
            "NSE".into()
        } else {
            exchange
        },
        product: product.into(),
        isin: Some(first(&sym, &["isin"])).filter(|x| !x.is_empty()),
        quantity: quantity as i32,
        t1_quantity: 0,
        average_price: round2(avg),
        ltp: round2(ltp),
        close_price: 0.0,
        pnl: round2(pnl),
        pnl_percentage: round2(pct),
        current_value: round2(cur),
    })
}

/// web `get_margin_data`: `d` is documented as an object but arrives as a
/// per-segment array in practice; numeric fields are then summed.
pub fn funds(d: &Value) -> Option<Funds> {
    let merged: Map<String, Value> = match d {
        Value::Object(m) => m.clone(),
        Value::Array(segments) => {
            if segments.is_empty() {
                return None;
            }
            let mut acc: Map<String, Value> = Map::new();
            for seg in segments.iter().filter_map(Value::as_object) {
                for (k, v) in seg {
                    if let Value::Number(n) = v {
                        let prev = acc.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                        acc.insert(
                            k.clone(),
                            serde_json::json!(prev + n.as_f64().unwrap_or(0.0)),
                        );
                    }
                }
            }
            acc
        }
        _ => return None,
    };
    let m = Value::Object(merged);
    let r = |k: &str| round2(f(&m, k));
    Some(Funds {
        available_cash: r("availMargin"),
        used_margin: r("marginUsed"),
        collateral: r("stockCollateral"),
        m2m_unrealized: r("unrealizedPnL"),
        m2m_realized: r("realizedPnl"),
        utilised_debits: r("marginUsed"),
        ..Default::default()
    })
}
