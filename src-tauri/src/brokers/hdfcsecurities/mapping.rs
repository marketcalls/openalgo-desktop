//! OpenAlgo <-> InvestRight translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`).
//!
//! InvestRight addresses an instrument by three fields: the PARENT exchange
//! (NSE, BSE, MCX), an `instrument_segment` (EQUITY, FUTIDX, OPTIDX, ...)
//! and `security_id` (the master's brsymbol). Book rows come back the same
//! way, so `(exchange, instrument_segment)` is what turns a row back into an
//! OpenAlgo exchange.

use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::master_contract::{format_strike, parse_oa_expiry};
use crate::brokers::common::symbols::{ContractQuery, SymToken, SymbolResolver};
use crate::brokers::types::*;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicI64, Ordering};

// ---------------------------------------------------------------------------
// Lenient row access (book rows are read as JSON objects)
// ---------------------------------------------------------------------------

pub fn s(row: &Value, k: &str) -> String {
    match row.get(k) {
        Some(Value::String(v)) => v.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Number, numeric string (thousands separators allowed) or null -> f64.
pub fn f(row: &Value, k: &str) -> f64 {
    match row.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(v)) => v.replace(',', "").trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Python `int(float(x))`.
pub fn i(row: &Value, k: &str) -> i64 {
    f(row, k) as i64
}

fn i32_of(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn round2(v: f64) -> f64 {
    crate::brokers::common::streaming::round2(v)
}

// ---------------------------------------------------------------------------
// Exchange codes
// ---------------------------------------------------------------------------

/// OpenAlgo exchange -> the REST `exchange` (parent) InvestRight orders want.
pub fn to_rest_exchange(oa: &str) -> &str {
    match oa {
        "NSE" | "NFO" | "CDS" | "NSE_INDEX" => "NSE",
        "BSE" | "BFO" | "BSE_INDEX" => "BSE",
        "MCX" => "MCX",
        other => other,
    }
}

/// `(REST exchange, instrument_segment)` -> OpenAlgo exchange (web
/// `_IR_SEGMENT_TO_OA`).
pub const SEGMENT_TO_OA: &[((&str, &str), &str)] = &[
    (("NSE", "EQUITY"), "NSE"),
    (("NSE", "FUTIDX"), "NFO"),
    (("NSE", "OPTIDX"), "NFO"),
    (("NSE", "FUTSTK"), "NFO"),
    (("NSE", "OPTSTK"), "NFO"),
    (("NSE", "FUTCUR"), "CDS"),
    (("NSE", "OPTCUR"), "CDS"),
    (("NSE", "UNDCUR"), "CDS"),
    (("BSE", "EQUITY"), "BSE"),
    (("BSE", "FUTIDX"), "BFO"),
    (("BSE", "OPTIDX"), "BFO"),
    (("BSE", "FUTSTK"), "BFO"),
    (("BSE", "OPTSTK"), "BFO"),
    (("MCX", "FUTCOM"), "MCX"),
    (("MCX", "OPTFUT"), "MCX"),
    (("MCX", "FUTIDX"), "MCX"),
    (("MCX", "OPTIDX"), "MCX"),
    (("MCX", "COM"), "MCX"),
];

/// InvestRight `(exchange, instrument_segment)` -> OpenAlgo exchange; the
/// parent exchange when the segment is absent or unknown.
pub fn to_oa_exchange(exchange: &str, segment: &str) -> String {
    let e = exchange.trim().to_ascii_uppercase();
    let sg = segment.trim().to_ascii_uppercase();
    if !sg.is_empty() {
        if let Some((_, oa)) = SEGMENT_TO_OA.iter().find(|((x, y), _)| *x == e && *y == sg) {
            return (*oa).to_string();
        }
    }
    e
}

/// Feed `scripId` prefix per OpenAlgo exchange (docs prefix table).
pub fn ws_prefix(oa: &str) -> &str {
    match oa {
        "CDS" => "NCD",
        other => other,
    }
}

/// `("NSE_INDEX", "26000")` -> `NSE_INDEX_26000`.
pub fn ws_scrip_id(oa: &str, token: &str) -> String {
    format!("{}_{}", ws_prefix(oa), token)
}

/// `NSE_INDEX_26000` -> `("NSE_INDEX", "26000")`; longest prefix wins.
pub fn from_ws_scrip_id(scrip_id: &str) -> Option<(&'static str, &str)> {
    const PREFIXES: &[(&str, &str)] = &[
        ("NSE_INDEX", "NSE_INDEX"),
        ("BSE_INDEX", "BSE_INDEX"),
        ("NSE", "NSE"),
        ("BSE", "BSE"),
        ("NFO", "NFO"),
        ("BFO", "BFO"),
        ("NCD", "CDS"),
        ("MCX", "MCX"),
    ];
    PREFIXES.iter().find_map(|(p, oa)| {
        scrip_id
            .strip_prefix(p)
            .and_then(|r| r.strip_prefix('_'))
            .map(|t| (*oa, t))
    })
}

fn is_index_exchange(oa: &str) -> bool {
    matches!(oa, "NSE_INDEX" | "BSE_INDEX")
}

// ---------------------------------------------------------------------------
// Instrument segment and underlying
// ---------------------------------------------------------------------------

pub const NFO_INDEX_UNDERLYINGS: &[&str] =
    &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY", "NIFTYNXT50"];
pub const BFO_INDEX_UNDERLYINGS: &[&str] = &["SENSEX", "SENSEX50", "BANKEX", "FOCIT"];
pub const MCX_INDEX_UNDERLYINGS: &[&str] = &["MCXBULLDEX", "MCXMETLDEX"];

/// Master row of a derivative's underlying: the index exchange first, then
/// the cash exchange (web `_underlying_row`).
pub fn underlying_row(symbols: &SymbolResolver, name: &str, oa: &str) -> Option<SymToken> {
    let candidates: &[&str] = match oa {
        "NFO" => &["NSE_INDEX", "NSE"],
        "BFO" => &["BSE_INDEX", "BSE"],
        _ => &[],
    };
    candidates.iter().find_map(|ex| symbols.by_symbol(ex, name))
}

fn is_index_underlying(name: &str, oa: &str, underlying: Option<&SymToken>) -> bool {
    if let Some(u) = underlying {
        return is_index_exchange(&u.exchange);
    }
    let set: &[&str] = match oa {
        "NFO" => NFO_INDEX_UNDERLYINGS,
        "BFO" => BFO_INDEX_UNDERLYINGS,
        "MCX" => MCX_INDEX_UNDERLYINGS,
        _ => &[],
    };
    set.contains(&name)
}

/// The `instrument_segment` an order must carry.
pub fn instrument_segment(
    oa: &str,
    instrument_type: &str,
    name: &str,
    underlying: Option<&SymToken>,
) -> &'static str {
    let option = matches!(instrument_type, "CE" | "PE");
    match oa {
        "NSE" | "BSE" | "NSE_INDEX" | "BSE_INDEX" => "EQUITY",
        "CDS" => {
            if option {
                "OPTCUR"
            } else {
                "FUTCUR"
            }
        }
        "MCX" => match (is_index_underlying(name, oa, underlying), option) {
            (true, true) => "OPTIDX",
            (true, false) => "FUTIDX",
            // Commodity options are written on the future.
            (false, true) => "OPTFUT",
            (false, false) => "FUTCOM",
        },
        _ => match (is_index_underlying(name, oa, underlying), option) {
            (true, true) => "OPTIDX",
            (true, false) => "FUTIDX",
            (false, true) => "OPTSTK",
            (false, false) => "FUTSTK",
        },
    }
}

/// `25-AUG-26` -> `20260825` (the order API's expiry format).
pub fn to_order_expiry(expiry: &str) -> String {
    parse_oa_expiry(expiry)
        .map(|d| d.format("%Y%m%d").to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Order parameters
// ---------------------------------------------------------------------------

/// OpenAlgo pricetype -> InvestRight `order_type` (same spelling).
pub fn map_order_type(p: PriceType) -> &'static str {
    p.as_str()
}

/// InvestRight `order_type` -> OpenAlgo pricetype.
pub fn reverse_order_type(t: &str) -> String {
    match t.trim().to_ascii_uppercase().as_str() {
        "MARKET" => "MARKET".into(),
        "LIMIT" => "LIMIT".into(),
        "SL" | "SL-L" => "SL".into(),
        "SLM" | "SL-M" => "SL-M".into(),
        _ => t.to_string(),
    }
}

fn is_cash_exchange(oa: &str) -> bool {
    matches!(oa, "NSE" | "BSE" | "NSE_INDEX" | "BSE_INDEX")
}

/// OpenAlgo product -> InvestRight product: DELIVERY / INTRADAY on cash,
/// OVERNIGHT / INTRADAY on derivatives.
pub fn map_product(p: Product, oa_exchange: &str) -> &'static str {
    match (p, is_cash_exchange(oa_exchange)) {
        (Product::Mis, _) => "INTRADAY",
        (_, true) => "DELIVERY",
        (_, false) => "OVERNIGHT",
    }
}

/// InvestRight product -> OpenAlgo product.
pub fn reverse_product(p: &str) -> Option<&'static str> {
    match p.trim().to_ascii_uppercase().as_str() {
        "DELIVERY" | "MTF" | "COLL-SELL" | "ENCASH" => Some("CNC"),
        "OVERNIGHT" => Some("NRML"),
        "INTRADAY" | "COVER" => Some("MIS"),
        _ => None,
    }
}

/// `Call` / `Put` (as the books return them) or `CE` / `PE` -> `CE` / `PE`.
pub fn reverse_option_type(t: &str) -> &'static str {
    match t.trim().to_ascii_uppercase().as_str() {
        "CALL" | "CE" => "CE",
        "PUT" | "PE" => "PE",
        _ => "",
    }
}

static LAST_REFERENCE: AtomicI64 = AtomicI64::new(0);

/// A strictly increasing millisecond counter (13 digits) so orders fired in
/// the same millisecond never share a reference (web
/// `_external_reference_number`).
pub fn external_reference_number() -> String {
    let now = chrono::Utc::now().timestamp_millis();
    let mut last = LAST_REFERENCE.load(Ordering::Relaxed);
    loop {
        let next = now.max(last + 1);
        match LAST_REFERENCE.compare_exchange_weak(last, next, Ordering::AcqRel, Ordering::Relaxed)
        {
            Ok(_) => return next.to_string(),
            Err(actual) => last = actual,
        }
    }
}

/// expiry / underlying / option fields of a derivative order.
fn derivative_fields(
    row: &SymToken,
    oa: &str,
    symbols: &SymbolResolver,
    out: &mut Map<String, Value>,
) {
    let t = row.instrument_type.to_ascii_uppercase();
    if !matches!(t.as_str(), "FUT" | "CE" | "PE") {
        return;
    }
    let underlying = underlying_row(symbols, &row.name, oa);
    out.insert("expiry_date".into(), json!(to_order_expiry(&row.expiry)));
    let usym = underlying
        .as_ref()
        .map(|u| u.br_symbol().to_string())
        .unwrap_or_else(|| row.name.clone());
    out.insert("underlying_symbol".into(), json!(usym));
    if t == "CE" || t == "PE" {
        out.insert("option_type".into(), json!(t));
        out.insert("strike_price".into(), json!(row.strike));
    }
}

/// Place-order body (web `transform_data`).
pub fn place_payload(o: &ResolvedOrder, symbols: &SymbolResolver) -> Value {
    let oa = o.exchange.as_str();
    let row = &o.instrument;
    let itype = row.instrument_type.to_ascii_uppercase();
    let underlying = if itype != "EQ" {
        underlying_row(symbols, &row.name, oa)
    } else {
        None
    };
    let mut m = Map::new();
    m.insert("exchange".into(), json!(to_rest_exchange(oa)));
    m.insert("security_id".into(), json!(o.brsymbol()));
    m.insert(
        "instrument_segment".into(),
        json!(instrument_segment(
            oa,
            &itype,
            &row.name,
            underlying.as_ref()
        )),
    );
    m.insert("transaction_type".into(), json!(o.action.as_str()));
    m.insert("product".into(), json!(map_product(o.product, oa)));
    m.insert("order_type".into(), json!(map_order_type(o.pricetype)));
    m.insert("quantity".into(), json!(o.quantity));
    m.insert("price".into(), json!(o.price));
    m.insert("trigger_price".into(), json!(o.trigger_price));
    m.insert("disclosed_quantity".into(), json!(o.disclosed_quantity));
    m.insert("validity".into(), json!("DAY"));
    m.insert("amo".into(), json!(false));
    m.insert(
        "external_reference_number".into(),
        json!(external_reference_number()),
    );
    derivative_fields(row, oa, symbols, &mut m);
    Value::Object(m)
}

/// Modify body: only the mutable fields (web `transform_modify_order_data`).
pub fn modify_payload(m: &ResolvedModify) -> Value {
    json!({
        "quantity": m.quantity,
        "order_type": map_order_type(m.pricetype),
        "validity": "DAY",
        "disclosed_quantity": m.disclosed_quantity,
        "product": map_product(m.product, m.exchange.as_str()),
        "price": m.price,
        "trigger_price": m.trigger_price,
        "amo": false,
    })
}

// ---------------------------------------------------------------------------
// Order status
// ---------------------------------------------------------------------------

/// InvestRight status (normalised: upper case, separators as single spaces)
/// -> OpenAlgo status (web `_STATUS_MAP`).
pub const STATUS_MAP: &[(&str, &str)] = &[
    ("TRADED", "complete"),
    ("EXECUTED", "complete"),
    ("COMPLETE", "complete"),
    ("COMPLETED", "complete"),
    ("FULLY EXECUTED", "complete"),
    ("REJECTED", "rejected"),
    ("CANCEL REJECTED", "rejected"),
    ("MODIFY REJECTED", "rejected"),
    ("CANCELLED", "cancelled"),
    ("CANCELED", "cancelled"),
    ("CANCEL CONFIRMED", "cancelled"),
    ("TRIGGER PENDING", "trigger pending"),
    ("SL TRIGGER PENDING", "trigger pending"),
    ("PENDING", "open"),
    ("OPEN", "open"),
    ("PLACED", "open"),
    ("ACCEPTED", "open"),
    ("CONFIRMED", "open"),
    ("RECEIVED", "open"),
    ("MODIFIED", "open"),
    ("MODIFY PENDING", "open"),
    ("CANCEL PENDING", "open"),
    ("PARTIALLY TRADED", "open"),
    ("PARTIAL TRADE", "open"),
    ("PUT ORDER REQ RECEIVED", "open"),
    ("VALIDATION PENDING", "open"),
    ("AFTER MARKET ORDER REQ RECEIVED", "open"),
    ("OPEN PENDING", "open"),
    ("TRANSIT", "open"),
];

pub fn normalise_status(s: &str) -> String {
    s.replace(['_', '-'], " ")
        .to_ascii_uppercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Mapped status; unknown values come back lowercased (web behaviour).
pub fn map_status(s: &str) -> String {
    let n = normalise_status(s);
    STATUS_MAP
        .iter()
        .find(|(k, _)| *k == n)
        .map(|(_, v)| (*v).to_string())
        .unwrap_or_else(|| n.to_ascii_lowercase())
}

/// Whether an order-book row can still be cancelled: the broker's own
/// `cancellation_allowed` flag first, the status table otherwise.
pub fn is_cancellable(row: &Value) -> bool {
    match s(row, "cancellation_allowed").to_ascii_uppercase().as_str() {
        "YES" | "Y" | "TRUE" => return true,
        "NO" | "N" | "FALSE" => return false,
        _ => {}
    }
    let n = normalise_status(&s(row, "status"));
    STATUS_MAP
        .iter()
        .any(|(k, v)| *k == n && matches!(*v, "open" | "trigger pending"))
}

// ---------------------------------------------------------------------------
// Symbol resolution for book rows
// ---------------------------------------------------------------------------

/// `30 APR 2024` or `27-JUN-24` -> `30APR24`.
pub fn compact_expiry(e: &str) -> String {
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    let up = e.replace('-', " ").to_ascii_uppercase();
    let parts: Vec<&str> = up.split_whitespace().collect();
    let [day, mon, year] = parts.as_slice() else {
        return String::new();
    };
    if !MONTHS.contains(mon) || !day.chars().all(|c| c.is_ascii_digit()) || year.len() < 2 {
        return String::new();
    }
    let Ok(d) = day.parse::<u32>() else {
        return String::new();
    };
    format!("{:02}{}{}", d, mon, &year[year.len() - 2..])
}

fn is_known_underlying(symbols: &SymbolResolver, name: &str, exchange: &str) -> bool {
    !symbols
        .contracts(&ContractQuery {
            exchange,
            underlying: name,
            ..Default::default()
        })
        .is_empty()
}

/// The underlying's display name for a row (web `_underlying_name`). An
/// unresolved `security_id` is only used once the master confirms it.
fn underlying_name(row: &Value, exchange: &str, verify: bool, symbols: &SymbolResolver) -> String {
    let name = {
        let u = s(row, "underlying_symbol");
        if u.is_empty() {
            s(row, "company_name")
        } else {
            u
        }
    };
    if !name.is_empty() {
        return name;
    }
    let c = s(row, "security_id");
    if c.is_empty() || c.chars().all(|ch| ch.is_ascii_digit()) {
        return String::new();
    }
    if verify && !is_known_underlying(symbols, &c, exchange) {
        return String::new();
    }
    c
}

/// Rebuild the OpenAlgo symbol from the parts a row spells out.
fn reconstruct_symbol(row: &Value, exchange: &str, symbols: &SymbolResolver) -> String {
    let expiry = compact_expiry(&s(row, "expiry_date"));
    let underlying = underlying_name(row, exchange, !expiry.is_empty(), symbols);
    if underlying.is_empty() {
        return String::new();
    }
    if expiry.is_empty() {
        return underlying;
    }
    let ot = reverse_option_type(&s(row, "option_type"));
    if ot.is_empty() {
        format!("{}{}FUT", underlying, expiry)
    } else {
        format!(
            "{}{}{}{}",
            underlying,
            expiry,
            format_strike(f(row, "strike_price")),
            ot
        )
    }
}

/// Broker row -> OpenAlgo symbol (web `_oa_symbol`): master lookup by
/// `security_id`, then reconstruction, then the raw id, then the ISIN.
pub fn oa_symbol(row: &Value, exchange: &str, symbols: &SymbolResolver) -> String {
    let sid = s(row, "security_id");
    if !sid.is_empty() {
        if let Some(r) = symbols.by_brsymbol(exchange, &sid) {
            return r.symbol;
        }
    }
    let rebuilt = reconstruct_symbol(row, exchange, symbols);
    if !rebuilt.is_empty() {
        return rebuilt;
    }
    if !sid.is_empty() {
        return sid;
    }
    s(row, "isin")
}

/// OpenAlgo exchange of a book row.
pub fn row_exchange(row: &Value) -> String {
    to_oa_exchange(&s(row, "exchange"), &s(row, "instrument_segment"))
}

fn row_product(row: &Value) -> String {
    let p = s(row, "product");
    reverse_product(&p).map(str::to_string).unwrap_or(p)
}

/// `data` as a list: a bare list, or the first list under an object
/// (positions nest under `net`).
pub fn unwrap_rows(data: &Value, key: Option<&str>) -> Vec<Value> {
    match data {
        Value::Array(a) => a.clone(),
        Value::Object(o) => key
            .and_then(|k| o.get(k))
            .and_then(Value::as_array)
            .or_else(|| o.values().find_map(Value::as_array))
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

pub fn map_orders(rows: &[Value], symbols: &SymbolResolver) -> Vec<Order> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| {
            let exchange = row_exchange(r);
            let quantity = i32_of(i(r, "quantity"));
            let filled = i32_of(i(r, "filled_quantity"));
            let status = map_status(&s(r, "status"));
            let reason = ["rejection_reason", "reason", "remarks"]
                .iter()
                .map(|k| s(r, k))
                .find(|v| !v.is_empty());
            let eoid = s(r, "exchange_order_id");
            Order {
                order_tag: None,
                order_id: s(r, "order_id"),
                exchange_order_id: (!eoid.is_empty()).then_some(eoid),
                symbol: oa_symbol(r, &exchange, symbols),
                exchange,
                side: s(r, "transaction_type").to_ascii_uppercase(),
                quantity,
                filled_quantity: filled,
                pending_quantity: if r.get("pending_quantity").is_some() {
                    i32_of(i(r, "pending_quantity"))
                } else if matches!(status.as_str(), "open" | "trigger pending") {
                    (quantity - filled).max(0)
                } else {
                    0
                },
                price: f(r, "price"),
                trigger_price: f(r, "trigger_price"),
                average_price: f(r, "average_price"),
                order_type: reverse_order_type(&s(r, "order_type")),
                product: row_product(r),
                rejection_reason: if status == "rejected" { reason } else { None },
                status,
                validity: {
                    let v = s(r, "validity");
                    if v.is_empty() {
                        "DAY".into()
                    } else {
                        v.to_ascii_uppercase()
                    }
                },
                order_timestamp: s(r, "order_timestamp"),
                exchange_timestamp: None,
            }
        })
        .collect()
}

pub fn map_trades(rows: &[Value], symbols: &SymbolResolver) -> Vec<Trade> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| {
            let exchange = row_exchange(r);
            let quantity = i(r, "filled_quantity");
            let avg = f(r, "average_price");
            // Prefer the broker's own traded value (multi-fill averaging).
            let value = if r.get("total_traded_value").is_some_and(|v| !v.is_null()) {
                f(r, "total_traded_value")
            } else {
                quantity as f64 * avg
            };
            Trade {
                order_tag: None,
                order_id: s(r, "order_id"),
                trade_id: s(r, "trade_id"),
                symbol: oa_symbol(r, &exchange, symbols),
                exchange,
                product: row_product(r),
                side: s(r, "transaction_type").to_ascii_uppercase(),
                quantity: i32_of(quantity),
                average_price: avg,
                trade_value: round2(value),
                timestamp: s(r, "fill_timestamp"),
            }
        })
        .collect()
}

/// Positions; `ltp` was merged into each row from `/fetch-ltp` beforehand.
pub fn map_positions(rows: &[Value], symbols: &SymbolResolver) -> Vec<Position> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| {
            let exchange = row_exchange(r);
            let net = i(r, "net_qty");
            let ltp = f(r, "ltp");
            let buy_value = f(r, "total_buy_value");
            let sell_value = f(r, "total_sell_value");
            let pnl = (sell_value - buy_value) + net as f64 * ltp;
            let average = if net > 0 {
                f(r, "average_buy_price")
            } else if net < 0 {
                f(r, "average_sell_price")
            } else {
                0.0
            };
            let realized = f(r, "realised_pl_overall_position");
            Position {
                symbol: oa_symbol(r, &exchange, symbols),
                exchange,
                product: row_product(r),
                quantity: i32_of(net),
                overnight_quantity: 0,
                average_price: round2(average),
                ltp: round2(ltp),
                pnl: round2(pnl),
                realized_pnl: realized,
                unrealized_pnl: if net != 0 && ltp != 0.0 {
                    round2((ltp - average) * net as f64)
                } else {
                    0.0
                },
                buy_quantity: i32_of(i(r, "total_buy_quantity")),
                buy_value,
                sell_quantity: i32_of(i(r, "total_sell_quantity")),
                sell_value,
            }
        })
        .collect()
}

/// Demat holdings (always cash equity, product CNC). `ltp` is the merged
/// live price, else the previous close.
pub fn map_holdings(rows: &[Value], symbols: &SymbolResolver) -> Vec<Holding> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| {
            let exchange = to_oa_exchange(&s(r, "exchange"), "EQUITY");
            let qty = i(r, "quantity");
            let avg = f(r, "average_price");
            let close = f(r, "close_price");
            let ltp = if r.get("ltp").is_some_and(|v| !v.is_null()) {
                f(r, "ltp")
            } else {
                close
            };
            let pnl = (ltp - avg) * qty as f64;
            let isin = s(r, "isin");
            Holding {
                symbol: oa_symbol(r, &exchange, symbols),
                exchange,
                product: "CNC".into(),
                isin: (!isin.is_empty()).then_some(isin),
                quantity: i32_of(qty),
                t1_quantity: i32_of(i(r, "t1_quantity")),
                average_price: round2(avg),
                ltp,
                close_price: close,
                pnl: round2(pnl),
                pnl_percentage: if avg != 0.0 {
                    round2((ltp - avg) / avg * 100.0)
                } else {
                    0.0
                },
                current_value: ltp * qty as f64,
            }
        })
        .collect()
}

/// Side of a close-out order for a net quantity.
pub fn exit_action(net: i64) -> Action {
    if net > 0 {
        Action::Sell
    } else {
        Action::Buy
    }
}
