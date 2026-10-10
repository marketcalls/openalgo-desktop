//! OpenAlgo <-> HDFC Sky vocabulary and book normalisation (web
//! `mapping/transform_data.py`, `mapping/order_data.py`,
//! `mapping/margin_data.py`, `api/funds.py`).

use crate::brokers::common::mapping::{PriceType, Product};
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Lenient field access (the web's `_float` / `_int` helpers)
// ---------------------------------------------------------------------------

/// String field; numbers are rendered, null and missing are empty.
pub fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.replace(',', "").trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Float field, 0 when missing or unreadable.
pub fn f(v: &Value, k: &str) -> f64 {
    num(v.get(k)).unwrap_or(0.0)
}

/// Integer field (`int(float(x))`), 0 when missing or unreadable.
pub fn i(v: &Value, k: &str) -> i64 {
    num(v.get(k)).map(|x| x as i64).unwrap_or(0)
}

fn i32_of(x: i64) -> i32 {
    i32::try_from(x).unwrap_or(if x < 0 { i32::MIN } else { i32::MAX })
}

// ---------------------------------------------------------------------------
// Exchanges
// ---------------------------------------------------------------------------

/// OpenAlgo exchange -> the code the REST endpoints (orders, charts) expect;
/// indices resolve to their parent cash exchange.
pub fn to_rest_exchange(oa: &str) -> &str {
    match oa {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        other => other,
    }
}

/// OpenAlgo exchange -> the code `/fetch-ltp` expects: indices keep their
/// own `NSE_INDEX` / `BSE_INDEX` codes (the parent code silently omits them).
pub fn to_ltp_exchange(oa: &str) -> &str {
    oa
}

/// HDFC Sky REST exchange code -> OpenAlgo exchange (`NCD` is currency).
pub fn to_oa_exchange(br: &str) -> String {
    let up = br.trim().to_ascii_uppercase();
    match up.as_str() {
        "NSE" | "BSE" | "NFO" | "BFO" | "CDS" | "MCX" => up,
        "NCD" => "CDS".into(),
        _ => br.to_string(),
    }
}

/// OpenAlgo exchange -> WebSocket scripId prefix.
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

/// Split a scripId back into `(oa_exchange, token)`; longest prefix wins so
/// `NSE_INDEX_` is not read as `NSE_`.
pub fn from_ws_scrip_id(id: &str) -> Option<(&'static str, &str)> {
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
        id.strip_prefix(p)
            .and_then(|rest| rest.strip_prefix('_'))
            .map(|tok| (*oa, tok))
    })
}

pub fn is_index_exchange(oa: &str) -> bool {
    matches!(oa, "NSE_INDEX" | "BSE_INDEX")
}

// ---------------------------------------------------------------------------
// Order parameters
// ---------------------------------------------------------------------------

pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "SL",
        PriceType::SlM => "SLM",
    }
}

/// HDFC Sky `order_type` -> OpenAlgo price type (unknown values pass through).
pub fn reverse_order_type(t: &str) -> String {
    match t.trim().to_ascii_uppercase().as_str() {
        "MARKET" => "MARKET".into(),
        "LIMIT" => "LIMIT".into(),
        "SL" => "SL".into(),
        "SLM" | "SL-M" => "SL-M".into(),
        _ => t.to_string(),
    }
}

/// HDFC Sky product -> OpenAlgo product (`MTF` is delivery). `None` when
/// unknown.
pub fn reverse_product(p: &str) -> Option<&'static str> {
    match p.trim().to_ascii_uppercase().as_str() {
        "CNC" | "MTF" => Some("CNC"),
        "NRML" => Some("NRML"),
        "MIS" => Some("MIS"),
        _ => None,
    }
}

/// Numeric product code of the margin calculator (proto `ProdType`).
pub fn product_code(p: Product) -> &'static str {
    match p {
        Product::Nrml => "0",
        Product::Cnc => "1",
        Product::Mis => "2",
    }
}

/// Known order statuses (proto `Status` enum, also what the REST book
/// returns) -> OpenAlgo lowercase status.
fn known_status(up: &str) -> Option<&'static str> {
    Some(match up {
        "COMPLETE" => "complete",
        "REJECTED" | "MODIFY_REJECTED" | "CANCEL_REJECTED" | "BRACKET_ORDER_REJECTED" => "rejected",
        "CANCELLED"
        | "CANCEL_CONFIRMED"
        | "BATCH_CANCEL_CONFIRMED"
        | "AMO_CANCEL_CONFIRMED"
        | "BRACKET_ORDER_CANCELLED" => "cancelled",
        "SL_TRIGGER_CONFIRMED" | "TRIGGER_PENDING" => "trigger pending",
        "ACCEPTED"
        | "CONFIRMED"
        | "PENDING"
        | "MODIFY_ACCEPTED"
        | "MODIFY_CONFIRMED"
        | "MODIFY_PENDING"
        | "CANCEL_ACCEPTED"
        | "CANCEL_PENDING"
        | "PARTIAL_TRADE"
        | "AMO_REQ_RECEIVED"
        | "AMO_REQ_CONFIRMED"
        | "AMO_REQ_MODIFIED"
        | "AMO_NEW_CONFIRMED"
        | "AMO_MODIFY_CONFIRMED"
        | "UNACCEPTED"
        | "EXCHANGE_RESPONSE_PENDING"
        | "RRM_PENDING_AT_EXCHANGE"
        | "RMS_VALIDATION_COMPLETED" => "open",
        _ => return None,
    })
}

/// Broker order status -> OpenAlgo lowercase status; unknown values are
/// lowercased (web `map_order_status`).
pub fn map_status(status: &str) -> String {
    match known_status(&status.trim().to_ascii_uppercase()) {
        Some(m) => m.to_string(),
        None => status.trim().to_ascii_lowercase(),
    }
}

/// Statuses cancel-all touches (web `CANCELLABLE_STATUSES`: the known
/// statuses that map to `open` or `trigger pending`).
pub fn is_cancellable(status: &str) -> bool {
    matches!(
        known_status(&status.trim().to_ascii_uppercase()),
        Some("open") | Some("trigger pending")
    )
}

// ---------------------------------------------------------------------------
// Chart series (web `api/data.py::_series_type`)
// ---------------------------------------------------------------------------

/// Index underlyings of NFO / BFO derivatives.
pub const NFO_INDEX_UNDERLYINGS: &[&str] =
    &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY", "NIFTYNXT50"];
pub const BFO_INDEX_UNDERLYINGS: &[&str] = &["SENSEX", "SENSEX50", "BANKEX", "FOCIT"];

/// The chart-data `seriesType` of a master row.
pub fn series_type(row: &SymToken) -> String {
    let ex = row.exchange.as_str();
    let inst = row.instrument_type.as_str();
    match ex {
        "NSE_INDEX" => "INDICES".into(),
        "BSE_INDEX" => "IDX".into(),
        "NSE" | "BSE" => match row.brsymbol.rsplit_once('-') {
            Some((_, series)) => series.to_string(),
            None => "EQ".into(),
        },
        "NFO" => {
            let idx = NFO_INDEX_UNDERLYINGS.contains(&row.name.as_str());
            match (inst == "FUT", idx) {
                (true, true) => "FUTIDX",
                (true, false) => "FUTSTK",
                (false, true) => "OPTIDX",
                (false, false) => "OPTSTK",
            }
            .into()
        }
        "BFO" => {
            let idx = BFO_INDEX_UNDERLYINGS.contains(&row.name.as_str());
            match (inst == "FUT", idx) {
                (true, true) => "IF",
                (true, false) => "SF",
                (false, true) => "IO",
                (false, false) => "SO",
            }
            .into()
        }
        "MCX" => if inst == "FUT" { "FUTCOM" } else { "OPTFUT" }.into(),
        "CDS" => if inst == "FUT" { "FUTCUR" } else { "OPTCUR" }.into(),
        _ => "EQ".into(),
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

/// Place-order body (web `transform_data`). `order_type` and `price` are the
/// already-protected values for MARKET / SL-M.
pub fn place_body(
    o: &ResolvedOrder,
    client_id: &str,
    order_type: &str,
    price: f64,
    user_order_id: i64,
) -> Value {
    json!({
        "exchange": to_rest_exchange(o.exchange.as_str()),
        "instrument_token": o.token(),
        "client_id": client_id,
        "order_type": order_type,
        "order_side": o.action.as_str(),
        "product": o.product.as_str(),
        "quantity": o.quantity,
        "price": price,
        "trigger_price": o.trigger_price,
        "disclosed_quantity": o.disclosed_quantity,
        "validity": "DAY",
        "device": "WEB",
        "execution_type": "REGULAR",
        "amo": false,
        "user_order_id": user_order_id,
    })
}

/// Modify body (web `transform_modify_order_data`): the place shape plus
/// `oms_order_id`, no side.
pub fn modify_body(m: &ResolvedModify, client_id: &str) -> Value {
    json!({
        "exchange": to_rest_exchange(m.exchange.as_str()),
        "instrument_token": m.token(),
        "client_id": client_id,
        "oms_order_id": m.order_id,
        "order_type": order_type(m.pricetype),
        "product": m.product.as_str(),
        "quantity": m.quantity,
        "price": m.price,
        "trigger_price": m.trigger_price,
        "disclosed_quantity": m.disclosed_quantity,
        "validity": "DAY",
        "execution_type": "REGULAR",
    })
}

/// Caller-generated numeric `user_order_id`: epoch ms truncated to 9 digits.
pub fn user_order_id(now_ms: i64) -> i64 {
    now_ms.rem_euclid(1_000_000_000)
}

/// Distinct `user_order_id`s (BR-05). Basket legs are placed ten at a time,
/// so several read the same millisecond and the web's scheme alone gives
/// them one id. Each id is the clock's (`user_order_id`) or, when that is
/// not past the last one handed out, the last one plus one, kept below
/// 1e9 like the clock's.
pub struct OrderIds {
    last: std::sync::atomic::AtomicI64,
}

impl Default for OrderIds {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderIds {
    pub const fn new() -> Self {
        Self {
            last: std::sync::atomic::AtomicI64::new(-1),
        }
    }

    /// The id for one order placed at `now_ms`.
    pub fn next(&self, now_ms: i64) -> i64 {
        use std::sync::atomic::Ordering;
        let clock = user_order_id(now_ms);
        let mut prev = self.last.load(Ordering::SeqCst);
        loop {
            let id = if clock > prev {
                clock
            } else {
                (prev + 1).rem_euclid(1_000_000_000)
            };
            match self
                .last
                .compare_exchange_weak(prev, id, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return id,
                Err(p) => prev = p,
            }
        }
    }
}

/// The process-wide allocator every HDFC Sky order takes its id from.
pub static ORDER_IDS: OrderIds = OrderIds::new();

/// Margin `segment` per OpenAlgo exchange (proto `Segment`).
pub fn margin_segment(oa: &str) -> &'static str {
    match oa {
        "NFO" | "BFO" => "FutOpt",
        "CDS" => "Currency",
        "MCX" => "Commodities",
        _ => "Capital",
    }
}

/// One margin request leg (web `build_margin_leg`). `underlying` is the spot
/// price rounded to a whole number (the calculator rejects decimals).
pub fn margin_leg(leg: &MarginLeg, row: &SymToken, underlying_price: f64) -> Value {
    json!({
        "segment": margin_segment(&row.exchange),
        "series": series_type(row),
        "exchange": row.br_exchange(),
        "side": leg.action.as_str(),
        "mode": "NEW",
        "symbol": row.br_symbol(),
        "underlying": underlying_price.round() as i64,
        "token": row.token,
        "quantity": leg.quantity.to_string(),
        "price": py_float_str(leg.price),
        "product": product_code(leg.product),
    })
}

/// Python `str(float)` for the values the web stringifies.
pub fn py_float_str(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

const MARGIN_COMPONENTS: &[&str] = &[
    "span",
    "exposure_margin",
    "premium_margin",
    "var_margin",
    "extreme_loss_margin",
    "delivery_margin",
    "additional_margin",
    "span_spread_margin",
    "somtier_margin",
];

fn sum_components(block: &Value) -> (f64, f64, f64) {
    if !block.is_object() {
        return (0.0, 0.0, 0.0);
    }
    let total: f64 =
        MARGIN_COMPONENTS.iter().map(|k| f(block, k)).sum::<f64>() - f(block, "premium_benefit");
    (
        total.max(0.0),
        f(block, "span"),
        f(block, "exposure_margin"),
    )
}

/// Margin `result` -> OpenAlgo margin (web `parse_margin_response`): the
/// netted `combined_margin`, or the sum of the legs when it is empty.
pub fn parse_margin(result: &Value) -> MarginResult {
    let combined = result
        .get("combined_margin")
        .cloned()
        .unwrap_or(Value::Null);
    let (mut total, mut span, mut exposure) = sum_components(&combined);
    let legs = result
        .get("individual_margin_values")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if total <= 0.0 && !legs.is_empty() {
        total = 0.0;
        span = 0.0;
        exposure = 0.0;
        for l in &legs {
            let (t, sp, ex) = sum_components(l);
            total += t;
            span += sp;
            exposure += ex;
        }
    }
    MarginResult {
        total_margin_required: round2(total),
        span_margin: round2(span),
        exposure_margin: round2(exposure),
    }
}

// ---------------------------------------------------------------------------
// Funds (web `api/funds.py`)
// ---------------------------------------------------------------------------

fn norm_label(l: &str) -> String {
    l.to_lowercase().split_whitespace().collect()
}

/// `data` of `/oapi/v1/funds/view` -> funds. Rows are `[label, value]`
/// pairs (or `{"0": label, "1": value}`); named top-level MTM fields win.
pub fn funds_from_view(data: &Value) -> Funds {
    let mut out = Funds::default();
    let rows = data
        .get("values")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for row in &rows {
        let (label, value) = match row {
            Value::Array(a) if a.len() >= 2 => (a[0].clone(), a[1].clone()),
            Value::Object(o) => (
                o.get("0").cloned().unwrap_or(Value::Null),
                o.get("1").cloned().unwrap_or(Value::Null),
            ),
            _ => continue,
        };
        let label = match &label {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let v = round2(num(Some(&value)).unwrap_or(0.0));
        match norm_label(&label).as_str() {
            "availablemargin" => out.available_cash = v,
            "marginused" => out.utilised_debits = v,
            "pledgebenefit" => out.collateral = v,
            "realizedmtm" => out.m2m_realized = v,
            "unrealizedmtm" => out.m2m_unrealized = v,
            _ => {}
        }
    }
    if let Some(v) = data.get("realized_mtm").filter(|v| !v.is_null()) {
        out.m2m_realized = round2(num(Some(v)).unwrap_or(0.0));
    }
    if let Some(v) = data.get("unrealized_mtm").filter(|v| !v.is_null()) {
        out.m2m_unrealized = round2(num(Some(v)).unwrap_or(0.0));
    }
    out.used_margin = out.utilised_debits;
    out
}

// ---------------------------------------------------------------------------
// Books (web `mapping/order_data.py`)
// ---------------------------------------------------------------------------

/// Rows of an envelope: `data` as a list, or `data[key]`.
pub fn unwrap_rows(payload: &Value, key: &str) -> Vec<Value> {
    match payload {
        Value::Array(a) => a.clone(),
        _ => match payload.get("data") {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Object(o)) => o
                .get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        },
    }
}

fn oa_symbol(symbols: &SymbolResolver, brsymbol: &str, exchange: &str) -> String {
    if brsymbol.is_empty() {
        return String::new();
    }
    symbols.oa_symbol_or_raw(brsymbol, exchange)
}

fn product_of(raw: &str) -> String {
    reverse_product(raw)
        .map(str::to_string)
        .unwrap_or_else(|| raw.to_string())
}

pub fn map_order(o: &Value, symbols: &SymbolResolver) -> Order {
    let exchange = to_oa_exchange(&s(o, "exchange"));
    let quantity = i(o, "quantity");
    let filled = ["filled_quantity", "traded_quantity", "fill_quantity"]
        .iter()
        .map(|k| i(o, k))
        .find(|v| *v != 0)
        .unwrap_or(0);
    let pending = if o.get("pending_quantity").is_some() {
        i(o, "pending_quantity")
    } else {
        (quantity - filled).max(0)
    };
    let status_raw = s(o, "order_status");
    let reason = ["reason", "rejection_reason", "rejected_reason"]
        .iter()
        .map(|k| s(o, k))
        .find(|v| !v.is_empty());
    let ts = {
        let t = s(o, "order_entry_time");
        if t.is_empty() {
            s(o, "exchange_time")
        } else {
            t
        }
    };
    let exch_ts = s(o, "exchange_time");
    let exch_oid = s(o, "exchange_order_id");
    let status = map_status(&status_raw);
    Order {
        order_tag: None,
        order_id: s(o, "oms_order_id"),
        exchange_order_id: (!exch_oid.is_empty()).then_some(exch_oid),
        symbol: oa_symbol(symbols, &s(o, "trading_symbol"), &exchange),
        exchange,
        side: s(o, "order_side").to_ascii_uppercase(),
        quantity: i32_of(quantity),
        filled_quantity: i32_of(filled),
        pending_quantity: i32_of(pending),
        price: f(o, "price"),
        trigger_price: f(o, "trigger_price"),
        average_price: ["average_trade_price", "average_price", "avg_price"]
            .iter()
            .map(|k| f(o, k))
            .find(|v| *v != 0.0)
            .unwrap_or(0.0),
        order_type: reverse_order_type(&s(o, "order_type")),
        product: product_of(&s(o, "product")),
        rejection_reason: if status == "rejected" { reason } else { None },
        status,
        validity: {
            let v = s(o, "validity");
            if v.is_empty() {
                "DAY".into()
            } else {
                v
            }
        },
        order_timestamp: ts,
        exchange_timestamp: (!exch_ts.is_empty()).then_some(exch_ts),
    }
}

pub fn map_orders(rows: &[Value], symbols: &SymbolResolver) -> Vec<Order> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| map_order(r, symbols))
        .collect()
}

pub fn map_trades(rows: &[Value], symbols: &SymbolResolver) -> Vec<Trade> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|t| {
            let exchange = to_oa_exchange(&s(t, "exchange"));
            let quantity = if t.get("trade_quantity").is_some() {
                i(t, "trade_quantity")
            } else {
                i(t, "filled_quantity")
            };
            let avg = if t.get("trade_price").is_some() {
                f(t, "trade_price")
            } else {
                f(t, "order_price")
            };
            let ts = {
                let x = s(t, "trade_time");
                if x.is_empty() {
                    s(t, "exchange_time")
                } else {
                    x
                }
            };
            Trade {
                order_tag: None,
                order_id: s(t, "oms_order_id"),
                trade_id: s(t, "trade_id"),
                symbol: oa_symbol(symbols, &s(t, "trading_symbol"), &exchange),
                exchange,
                product: product_of(&s(t, "product")),
                side: s(t, "order_side").to_ascii_uppercase(),
                quantity: i32_of(quantity),
                average_price: avg,
                trade_value: round2(quantity as f64 * avg),
                timestamp: ts,
            }
        })
        .collect()
}

pub fn map_position(p: &Value, symbols: &SymbolResolver) -> Position {
    let exchange = to_oa_exchange(&s(p, "exchange"));
    let net = i(p, "net_quantity");
    let multiplier = {
        let m = f(p, "multiplier");
        if p.get("multiplier").is_none() || m == 0.0 {
            1.0
        } else {
            m
        }
    };
    let ltp = f(p, "ltp");
    let buy_amount = f(p, "buy_amount");
    let sell_amount = f(p, "sell_amount");
    let pnl = (sell_amount - buy_amount) + net as f64 * ltp * multiplier;
    let avg = if net > 0 {
        f(p, "average_buy_price")
    } else if net < 0 {
        f(p, "average_sell_price")
    } else {
        f(p, "average_price")
    };
    Position {
        symbol: oa_symbol(symbols, &s(p, "trading_symbol"), &exchange),
        exchange,
        product: product_of(&s(p, "product")),
        quantity: i32_of(net),
        overnight_quantity: i32_of(i(p, "cf_buy_quantity") - i(p, "cf_sell_quantity")),
        average_price: round2(avg),
        ltp: round2(ltp),
        pnl: round2(pnl),
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        buy_quantity: i32_of(i(p, "buy_quantity")),
        buy_value: buy_amount,
        sell_quantity: i32_of(i(p, "sell_quantity")),
        sell_value: sell_amount,
    }
}

pub fn map_positions(rows: &[Value], symbols: &SymbolResolver) -> Vec<Position> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|r| map_position(r, symbols))
        .collect()
}

pub fn map_holdings(rows: &[Value], symbols: &SymbolResolver) -> Vec<Holding> {
    rows.iter()
        .filter(|r| r.is_object())
        .map(|h| {
            let exchange = to_oa_exchange(&s(h, "exchange"));
            // The row's own symbol is series-free; the master stores the
            // broker form from `instrument_details`.
            let details = h.get("instrument_details").cloned().unwrap_or(Value::Null);
            let mut br = s(&details, "trading_symbol");
            if br.is_empty() {
                br = s(h, "trading_symbol");
            }
            let quantity = i(h, "quantity");
            let avg = f(h, "buy_avg");
            let ltp = f(h, "ltp");
            let pnl = (ltp - avg) * quantity as f64;
            let pct = if avg != 0.0 {
                (ltp - avg) / avg * 100.0
            } else {
                0.0
            };
            let isin = {
                let x = s(h, "isin");
                if x.is_empty() {
                    s(&details, "isin")
                } else {
                    x
                }
            };
            Holding {
                symbol: oa_symbol(symbols, &br, &exchange),
                exchange,
                product: "CNC".into(),
                isin: (!isin.is_empty()).then_some(isin),
                quantity: i32_of(quantity),
                t1_quantity: i32_of(i(h, "t1_quantity")),
                average_price: round2(avg),
                ltp,
                close_price: f(h, "close_price"),
                pnl: round2(pnl),
                pnl_percentage: round2(pct),
                current_value: ltp * quantity as f64,
            }
        })
        .collect()
}

/// Whether an OpenAlgo exchange's instruments carry open interest.
pub fn has_oi(oa: &str) -> bool {
    matches!(oa, "NFO" | "BFO" | "CDS" | "MCX")
}
