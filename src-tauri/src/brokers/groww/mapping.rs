//! OpenAlgo <-> Groww field maps and book normalisers (web
//! `mapping/transform_data.py`, `mapping/order_data.py`).

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde::Deserialize;

pub const SEGMENT_CASH: &str = "CASH";
pub const SEGMENT_FNO: &str = "FNO";

/// OpenAlgo exchange -> Groww `exchange` (`NFO` trades on `NSE`).
pub fn groww_exchange(exchange: &str) -> &'static str {
    match exchange {
        "BSE" | "BFO" | "BSE_INDEX" => "BSE",
        _ => "NSE",
    }
}

/// OpenAlgo exchange -> Groww `segment`.
pub fn groww_segment(exchange: &str) -> &'static str {
    match exchange {
        "NFO" | "BFO" => SEGMENT_FNO,
        _ => SEGMENT_CASH,
    }
}

/// OpenAlgo price type -> Groww `order_type`.
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "STOP_LOSS_LIMIT",
        PriceType::SlM => "STOP_LOSS_MARKET",
    }
}

/// Groww `order_type` -> OpenAlgo price type. The web maps only
/// `STOP_LOSS`; `STOP_LOSS_LIMIT` (what it sends) is mapped too (quirk 9.6).
pub fn reverse_order_type(t: &str) -> String {
    match t {
        "STOP_LOSS" | "STOP_LOSS_LIMIT" => "SL".into(),
        "STOP_LOSS_MARKET" => "SL-M".into(),
        other => other.into(),
    }
}

/// OpenAlgo product -> Groww product (same names).
pub fn product(p: Product) -> &'static str {
    p.as_str()
}

/// Groww product -> OpenAlgo product.
pub fn reverse_product(p: &str) -> String {
    match p {
        "INTRADAY" => "MIS".into(),
        "MARGIN" => "NRML".into(),
        other => other.into(),
    }
}

/// Groww `order_status` -> OpenAlgo status. Unknown statuses read as
/// `open` like the web's `map_order_data`, except `FAILED` (rejected) and
/// `DELIVERY_AWAITED` (executed), which the web also reported as `open`.
pub fn map_status(s: &str) -> String {
    match s.trim().to_ascii_uppercase().as_str() {
        "NEW" | "ACKED" | "OPEN" | "APPROVED" => "open",
        "TRIGGER_PENDING" => "trigger pending",
        "EXECUTED" | "COMPLETED" | "DELIVERY_AWAITED" => "complete",
        "CANCELLED" => "cancelled",
        "REJECTED" | "FAILED" => "rejected",
        _ => "open",
    }
    .to_string()
}

/// Statuses the web's `cancel_all_orders_api` cancels.
pub fn is_cancellable(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_uppercase().as_str(),
        "OPEN"
            | "PENDING"
            | "TRIGGER_PENDING"
            | "PLACED"
            | "PENDING_ORDER"
            | "NEW"
            | "ACKED"
            | "APPROVED"
            | "MODIFICATION_REQUESTED"
    )
}

/// OpenAlgo exchange of a Groww row. The web guessed `NFO` from any `C`
/// or `P` in the symbol (quirk 9.1); the segment is authoritative.
pub fn oa_exchange(exchange: &str, segment: &str) -> String {
    let fno = segment.eq_ignore_ascii_case("FNO")
        || segment.eq_ignore_ascii_case("F&O")
        || segment.eq_ignore_ascii_case("FO");
    match (exchange, fno) {
        ("BSE", true) | ("BSE_FO", _) | ("BFO", _) => "BFO".into(),
        ("NSE", true) | ("NSE_FO", _) | ("NFO", _) => "NFO".into(),
        ("BSE_EQ", _) | ("BSE", false) => "BSE".into(),
        _ => "NSE".into(),
    }
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

fn split_alpha(s: &str) -> (&str, &str) {
    let i = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_uppercase())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s.split_at(i)
}

fn month(mm: &str) -> Option<&'static str> {
    let m: usize = mm.parse().ok()?;
    MONTHS.get(m.checked_sub(1)?).copied()
}

/// Web regex fallbacks for a derivative symbol the master does not know
/// (`order_api.py:258-339`): `NIFTY250515 24500 CE`-style compact codes
/// `[NAME][YY][MM][DD][STRIKE][CE|PE]` and `[NAME][YY][MM][DD][FUT]`.
pub fn derivative_fallback(br: &str) -> Option<String> {
    let (name, rest) = split_alpha(br);
    if name.is_empty() || rest.len() < 6 {
        return None;
    }
    let (yy, mm, dd) = (&rest[0..2], &rest[2..4], &rest[4..6]);
    if !rest[..6].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mon = month(mm)?;
    let tail = &rest[6..];
    if tail.is_empty() || tail == "FUT" {
        return Some(format!("{}{}{}{}FUT", name, dd, mon, yy));
    }
    for opt in ["CE", "PE"] {
        if let Some(strike) = tail.strip_suffix(opt) {
            if !strike.is_empty() && strike.bytes().all(|b| b.is_ascii_digit()) {
                return Some(format!("{}{}{}{}{}{}", name, dd, mon, yy, strike, opt));
            }
        }
    }
    None
}

/// OpenAlgo symbol for a Groww trading symbol on an OpenAlgo exchange:
/// master lookup, then (derivatives) the web's regex fallbacks, then the
/// Groww symbol unchanged.
pub fn oa_symbol(symbols: &SymbolResolver, brsymbol: &str, exchange: &str) -> String {
    if let Some(s) = symbols.oa_symbol(brsymbol, exchange) {
        return s;
    }
    if exchange == "NSE" || exchange == "BSE" {
        // An index traded in the cash book (rare) still resolves.
        let idx = if exchange == "NSE" {
            "NSE_INDEX"
        } else {
            "BSE_INDEX"
        };
        if let Some(s) = symbols.oa_symbol(brsymbol, idx) {
            return s;
        }
    }
    if matches!(exchange, "NFO" | "BFO") {
        if let Some(s) = derivative_fallback(brsymbol) {
            return s;
        }
    }
    brsymbol.to_string()
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GrowwOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub groww_order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub segment: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub validity: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub filled_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub remaining_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub trigger_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_fill_price: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub created_at: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange_time: String,
    #[serde(deserialize_with = "string_lenient")]
    pub remark: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_reference_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange_order_id: String,
}

fn clamp_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

pub fn map_order(o: &GrowwOrder, symbols: &SymbolResolver) -> Order {
    let exchange = oa_exchange(&o.exchange, &o.segment);
    let status = map_status(&o.order_status);
    let pending = if o.remaining_quantity > 0 {
        o.remaining_quantity
    } else if matches!(status.as_str(), "open" | "trigger pending") {
        (o.quantity - o.filled_quantity).max(0)
    } else {
        0
    };
    Order {
        order_id: o.groww_order_id.clone(),
        exchange_order_id: (!o.exchange_order_id.is_empty()).then(|| o.exchange_order_id.clone()),
        symbol: oa_symbol(symbols, &o.trading_symbol, &exchange),
        exchange,
        side: o.transaction_type.clone(),
        quantity: clamp_i32(o.quantity),
        filled_quantity: clamp_i32(o.filled_quantity),
        pending_quantity: clamp_i32(pending),
        price: o.price,
        trigger_price: o.trigger_price,
        average_price: o.average_fill_price,
        order_type: reverse_order_type(&o.order_type),
        product: reverse_product(&o.product),
        rejection_reason: (status == "rejected" && !o.remark.is_empty()).then(|| o.remark.clone()),
        status,
        validity: if o.validity.is_empty() {
            "DAY".into()
        } else {
            o.validity.clone()
        },
        order_timestamp: o.created_at.clone(),
        exchange_timestamp: (!o.exchange_time.is_empty()).then(|| o.exchange_time.clone()),
    }
}

pub fn map_orders(orders: &[GrowwOrder], symbols: &SymbolResolver) -> Vec<Order> {
    orders.iter().map(|o| map_order(o, symbols)).collect()
}

/// Web `calculate_order_statistics` counts.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OrderStats {
    pub total_buy_orders: usize,
    pub total_sell_orders: usize,
    pub total_completed_orders: usize,
    pub total_open_orders: usize,
    pub total_rejected_orders: usize,
}

pub fn order_stats(orders: &[GrowwOrder]) -> OrderStats {
    let mut s = OrderStats::default();
    for o in orders {
        match o.transaction_type.as_str() {
            "BUY" => s.total_buy_orders += 1,
            "SELL" => s.total_sell_orders += 1,
            _ => {}
        }
        match o.order_status.as_str() {
            "EXECUTED" | "COMPLETED" => s.total_completed_orders += 1,
            "NEW" | "ACKED" | "APPROVED" | "OPEN" => s.total_open_orders += 1,
            "REJECTED" => s.total_rejected_orders += 1,
            _ => {}
        }
    }
    s
}

/// Orders the trade book reads trades for (web `get_trade_book`).
pub fn has_fills(o: &GrowwOrder) -> bool {
    let s = o.order_status.to_ascii_uppercase();
    matches!(
        s.as_str(),
        "EXECUTED" | "COMPLETED" | "FILLED" | "PARTIAL" | "COMPLETE"
    ) || s.contains("EXECUT")
        || s.contains("FILL")
        || s.contains("COMPLET")
        || o.filled_quantity > 0
}

// ---------------------------------------------------------------------------
// Trades
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GrowwTrade {
    #[serde(deserialize_with = "string_lenient")]
    pub groww_trade_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub groww_order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange_trade_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    /// Rupees; never scaled (web `test_groww_tradebook_price.py`).
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub trade_status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub segment: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub created_at: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trade_date_time: String,
}

pub fn map_trade(t: &GrowwTrade, order: &GrowwOrder, symbols: &SymbolResolver) -> Trade {
    let exchange = oa_exchange(
        if t.exchange.is_empty() {
            &order.exchange
        } else {
            &t.exchange
        },
        if t.segment.is_empty() {
            &order.segment
        } else {
            &t.segment
        },
    );
    let br = if t.trading_symbol.is_empty() {
        &order.trading_symbol
    } else {
        &t.trading_symbol
    };
    Trade {
        order_id: if t.groww_order_id.is_empty() {
            order.groww_order_id.clone()
        } else {
            t.groww_order_id.clone()
        },
        trade_id: t.groww_trade_id.clone(),
        symbol: oa_symbol(symbols, br, &exchange),
        exchange,
        product: reverse_product(if t.product.is_empty() {
            &order.product
        } else {
            &t.product
        }),
        side: if t.transaction_type.is_empty() {
            order.transaction_type.clone()
        } else {
            t.transaction_type.clone()
        },
        quantity: clamp_i32(t.quantity),
        average_price: t.price,
        trade_value: t.price * t.quantity as f64,
        timestamp: if t.trade_date_time.is_empty() {
            t.created_at.clone()
        } else {
            t.trade_date_time.clone()
        },
    }
}

/// A trade made up from an executed order when Groww has no trade rows for
/// it (web: 404 / empty trade list with `filled_quantity > 0`).
pub fn synthetic_trade(order: &GrowwOrder) -> GrowwTrade {
    let qty = if order.filled_quantity > 0 {
        order.filled_quantity
    } else {
        order.quantity
    };
    GrowwTrade {
        groww_trade_id: format!("synthetic_{}", order.groww_order_id),
        groww_order_id: order.groww_order_id.clone(),
        trading_symbol: order.trading_symbol.clone(),
        quantity: qty,
        price: if order.average_fill_price > 0.0 {
            order.average_fill_price
        } else {
            order.price
        },
        trade_status: "EXECUTED".into(),
        exchange: order.exchange.clone(),
        segment: order.segment.clone(),
        product: order.product.clone(),
        transaction_type: order.transaction_type.clone(),
        created_at: order.created_at.clone(),
        trade_date_time: if order.exchange_time.is_empty() {
            order.created_at.clone()
        } else {
            order.exchange_time.clone()
        },
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Positions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GrowwPosition {
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub segment: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbol_isin: String,
    /// Net quantity when Groww sends it.
    pub quantity: Option<serde_json::Value>,
    #[serde(deserialize_with = "i64_lenient")]
    pub credit_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub carry_forward_credit_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub debit_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub carry_forward_debit_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub net_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub credit_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub debit_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub carry_forward_credit_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub carry_forward_debit_price: f64,
}

/// Web `get_positions` derivations: buy/sell quantities include carry
/// forward. Groww documents every position price in rupees (`net_price`,
/// `credit_price`, `debit_price`), so they are carried through unchanged
/// (web #2173 removed the old paise conversions). A non-finite price reads
/// as 0, and a position with nothing sold reports a sell price of 0.
pub fn map_position(p: &GrowwPosition, segment: &str, symbols: &SymbolResolver) -> Position {
    let buy_qty = p.credit_quantity + p.carry_forward_credit_quantity;
    let sell_qty = p.debit_quantity + p.carry_forward_debit_quantity;
    let net = match &p.quantity {
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0) as i64,
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<f64>()
            .map(|v| v as i64)
            .unwrap_or(buy_qty - sell_qty),
        _ => buy_qty - sell_qty,
    };
    let rupees = |v: f64| if v.is_finite() { v } else { 0.0 };
    let avg = rupees(p.net_price);
    let buy_price = rupees(p.credit_price);
    let sell_price = rupees(p.debit_price).max(0.0);
    let seg = if p.segment.is_empty() {
        segment
    } else {
        &p.segment
    };
    let exchange = oa_exchange(&p.exchange, seg);
    Position {
        symbol: oa_symbol(symbols, &p.trading_symbol, &exchange),
        exchange,
        product: reverse_product(&p.product),
        quantity: clamp_i32(net),
        overnight_quantity: clamp_i32(
            p.carry_forward_credit_quantity - p.carry_forward_debit_quantity,
        ),
        average_price: avg,
        // Groww's position book carries no LTP or P&L (web sends 0).
        ltp: 0.0,
        pnl: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        buy_quantity: clamp_i32(buy_qty),
        buy_value: buy_price * buy_qty as f64,
        sell_quantity: clamp_i32(sell_qty),
        sell_value: sell_price * sell_qty as f64,
    }
}

/// Whether a refused position read only says the book is empty (web
/// `says_no_positions`).
pub fn says_no_positions(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("no position") || m.contains("no data") || m.contains("not found")
}

// ---------------------------------------------------------------------------
// Holdings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GrowwHolding {
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub demat_free_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub demat_locked_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub groww_locked_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub pledge_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub t1_quantity: i64,
}

/// Groww holdings carry no price or P&L (web test comment), so `ltp` and
/// `pnl` are 0 like the web; the symbol resolves on NSE, then BSE.
pub fn map_holding(h: &GrowwHolding, symbols: &SymbolResolver) -> Holding {
    let (symbol, exchange) = match symbols.oa_symbol(&h.trading_symbol, "NSE") {
        Some(s) => (s, "NSE"),
        None => match symbols.oa_symbol(&h.trading_symbol, "BSE") {
            Some(s) => (s, "BSE"),
            None => (h.trading_symbol.clone(), "NSE"),
        },
    };
    Holding {
        symbol,
        exchange: exchange.into(),
        product: "CNC".into(),
        isin: (!h.isin.is_empty()).then(|| h.isin.clone()),
        quantity: clamp_i32(h.quantity),
        t1_quantity: clamp_i32(h.t1_quantity),
        average_price: h.average_price,
        ltp: 0.0,
        close_price: 0.0,
        pnl: 0.0,
        pnl_percentage: 0.0,
        current_value: h.average_price * h.quantity as f64,
    }
}

/// `Exchange` for the cash or derivatives side of a segment check.
pub fn segment_of(exchange: Exchange) -> &'static str {
    groww_segment(exchange.as_str())
}

/// Web `order_reference_id` rules: keep `[A-Za-z0-9-]`, at most two
/// hyphens, left-justified with `0` to 8, truncated to 20.
pub fn sanitize_reference_id(raw: &str) -> String {
    let mut hyphens = 0;
    let mut out: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .filter(|c| {
            if *c == '-' {
                hyphens += 1;
                hyphens <= 2
            } else {
                true
            }
        })
        .collect();
    while out.len() < 8 {
        out.push('0');
    }
    out.truncate(20);
    out
}

/// `YYYYMMDD-<8 hex>` (17 chars), as the web generates when the request
/// has none.
pub fn new_reference_id(today: chrono::NaiveDate) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    sanitize_reference_id(&format!("{}-{}", today.format("%Y%m%d"), &id[..8]))
}
