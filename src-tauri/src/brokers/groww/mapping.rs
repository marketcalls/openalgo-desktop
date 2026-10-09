//! OpenAlgo <-> Groww field maps and book normalisers (web
//! `mapping/transform_data.py`, `mapping/order_data.py`, aligned with
//! Groww's API docs in #2194).

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::mapping::{Exchange, PriceType, Product, Validity};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use serde::Deserialize;

pub const SEGMENT_CASH: &str = "CASH";
pub const SEGMENT_FNO: &str = "FNO";

/// Why an order on this exchange cannot go to Groww (web
/// `_unsupported_exchange`).
pub fn unsupported_exchange(exchange: &str) -> AppError {
    AppError::Validation(format!(
        "Groww's trading API does not support the {} exchange. Orders can be placed on NSE, BSE, NFO and BFO only.",
        exchange
    ))
}

/// OpenAlgo exchange -> Groww `exchange` for orders (web
/// `map_exchange_type`): NSE/BSE/NFO/BFO only, never a default.
pub fn order_exchange(exchange: &str) -> Result<&'static str> {
    match exchange {
        "NSE" | "NFO" => Ok("NSE"),
        "BSE" | "BFO" => Ok("BSE"),
        other => Err(unsupported_exchange(other)),
    }
}

/// OpenAlgo exchange -> Groww `segment` for orders (web
/// `map_segment_type`).
pub fn order_segment(exchange: &str) -> Result<&'static str> {
    match exchange {
        "NSE" | "BSE" => Ok(SEGMENT_CASH),
        "NFO" | "BFO" => Ok(SEGMENT_FNO),
        other => Err(unsupported_exchange(other)),
    }
}

/// Validity: Groww's annexure lists DAY only; anything else is refused
/// rather than quietly changed (web `map_validity`).
pub fn validity(v: Validity) -> Result<&'static str> {
    match v {
        Validity::Day => Ok("DAY"),
        other => Err(AppError::Validation(format!(
            "Groww accepts DAY validity only; {} is not supported.",
            other.as_str()
        ))),
    }
}

/// OpenAlgo exchange -> Groww `exchange` for market data: indices are on
/// their own exchange's CASH segment.
pub fn groww_exchange(exchange: &str) -> &'static str {
    match exchange {
        "BSE" | "BFO" | "BSE_INDEX" => "BSE",
        _ => "NSE",
    }
}

/// OpenAlgo exchange -> Groww `segment` for market data.
pub fn groww_segment(exchange: &str) -> &'static str {
    match exchange {
        "NFO" | "BFO" => SEGMENT_FNO,
        _ => SEGMENT_CASH,
    }
}

/// Exchanges Groww has market data for (web quotes, depth, multiquotes).
pub fn check_data_exchange(exchange: &str) -> Result<()> {
    if matches!(
        exchange,
        "NSE" | "BSE" | "NFO" | "BFO" | "NSE_INDEX" | "BSE_INDEX"
    ) {
        return Ok(());
    }
    Err(AppError::Validation(format!(
        "Groww does not provide market data for the {} exchange. Supported: NSE, BSE, NFO, BFO, NSE_INDEX and BSE_INDEX.",
        exchange
    )))
}

/// OpenAlgo price type -> Groww `order_type` (annexure "Order Type":
/// MARKET, LIMIT, SL, SL_M).
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "SL",
        PriceType::SlM => "SL_M",
    }
}

/// Groww `order_type` -> OpenAlgo price type (web `GROWW_PRICETYPE_MAP`);
/// anything else as sent.
pub fn reverse_order_type(t: &str) -> String {
    match t {
        "SL_M" => "SL-M".into(),
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

/// Groww `order_status` -> OpenAlgo status (web `GROWW_ORDER_STATUS_MAP`,
/// every status of Groww's annexure plus `OPEN`). A requested cancel or
/// modify is still working until Groww confirms it. Unknown statuses are
/// shown as sent, lowercased.
pub fn map_status(s: &str) -> String {
    let up = s.trim().to_ascii_uppercase();
    match up.as_str() {
        "NEW"
        | "ACKED"
        | "OPEN"
        | "APPROVED"
        | "MODIFICATION_REQUESTED"
        | "CANCELLATION_REQUESTED" => "open".into(),
        "TRIGGER_PENDING" => "trigger pending".into(),
        "EXECUTED" | "DELIVERY_AWAITED" | "COMPLETED" => "complete".into(),
        "CANCELLED" => "cancelled".into(),
        "REJECTED" | "FAILED" => "rejected".into(),
        _ => {
            tracing::warn!("Unmapped Groww order status: {:?}", s);
            s.trim().to_ascii_lowercase()
        }
    }
}

/// Statuses the web's `cancel_all_orders_api` cancels.
pub fn is_cancellable(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_uppercase().as_str(),
        "NEW" | "ACKED" | "TRIGGER_PENDING" | "APPROVED" | "OPEN" | "MODIFICATION_REQUESTED"
    )
}

/// OpenAlgo exchange for a Groww exchange (NSE/BSE) and segment (CASH/FNO)
/// (web `openalgo_exchange`): F&O goes to NFO/BFO; anything else as sent.
pub fn oa_exchange(exchange: &str, segment: &str) -> String {
    if segment == SEGMENT_FNO {
        return match exchange {
            "NSE" => "NFO".into(),
            "BSE" => "BFO".into(),
            other => other.into(),
        };
    }
    exchange.into()
}

/// OpenAlgo symbol for a Groww trading symbol: the master contract, else
/// Groww's symbol unchanged (web `get_oa_symbol(..) or groww_symbol`).
pub fn oa_symbol(symbols: &SymbolResolver, brsymbol: &str, exchange: &str) -> String {
    symbols
        .oa_symbol(brsymbol, exchange)
        .unwrap_or_else(|| brsymbol.to_string())
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

/// One order-book row (web `map_order_data` + `transform_order_data`).
/// The exchange is Groww's exchange + segment, the symbol comes from the
/// master contract. A TRIGGER_PENDING stop-loss is shown as `open`, as
/// Zerodha does, so the Order Book offers Cancel/Modify for it.
pub fn map_order(o: &GrowwOrder, symbols: &SymbolResolver) -> Order {
    let exchange = oa_exchange(&o.exchange, &o.segment);
    let detailed = map_status(&o.order_status);
    let pending = if o.remaining_quantity > 0 {
        o.remaining_quantity
    } else if matches!(detailed.as_str(), "open" | "trigger pending") {
        (o.quantity - o.filled_quantity).max(0)
    } else {
        0
    };
    let status = if detailed == "trigger pending" {
        "open".to_string()
    } else {
        detailed
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

/// Web `calculate_order_statistics` counts, read from the mapped status
/// (open includes orders waiting on their trigger).
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
        match map_status(&o.order_status).as_str() {
            "complete" => s.total_completed_orders += 1,
            "open" | "trigger pending" => s.total_open_orders += 1,
            "rejected" => s.total_rejected_orders += 1,
            _ => {}
        }
    }
    s
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

/// One fill of an order read from `/v1/order/trades/{id}` with `segment`
/// (web `get_order_trades` + `transform_tradebook_data`): exchange from the
/// trade's exchange + segment, symbol from the master contract.
pub fn map_trade(t: &GrowwTrade, order_id: &str, segment: &str, symbols: &SymbolResolver) -> Trade {
    let seg = if t.segment.is_empty() {
        segment
    } else {
        &t.segment
    };
    let exchange = oa_exchange(&t.exchange, seg);
    Trade {
        order_id: if t.groww_order_id.is_empty() {
            order_id.to_string()
        } else {
            t.groww_order_id.clone()
        },
        trade_id: t.groww_trade_id.clone(),
        symbol: oa_symbol(symbols, &t.trading_symbol, &exchange),
        exchange,
        product: reverse_product(&t.product),
        side: t.transaction_type.clone(),
        quantity: clamp_i32(t.quantity),
        average_price: t.price,
        trade_value: t.price * t.quantity as f64,
        timestamp: t.trade_date_time.clone(),
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
    /// Groww's documented realised P&L of the position, rupees.
    #[serde(deserialize_with = "f64_lenient")]
    pub realised_pnl: f64,
}

/// `EXCHANGE_TRADINGSYMBOL` key of `/v1/live-data/ltp` and `/ohlc`.
pub fn ltp_key(groww_exchange: &str, trading_symbol: &str) -> String {
    format!("{}_{}", groww_exchange, trading_symbol)
}

/// One Groww position (06-portfolio "Get User Positions") in OpenAlgo
/// terms (web `_position_row`): buy/sell quantities include carry forward;
/// every price is rupees as Groww documents them (web #2173); P&L starts
/// as `realised_pnl` and `attach_ltp` adds the open quantity's move.
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
    let realised = rupees(p.realised_pnl);
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
        ltp: 0.0,
        pnl: realised,
        realized_pnl: realised,
        unrealized_pnl: 0.0,
        buy_quantity: clamp_i32(buy_qty),
        buy_value: buy_price * buy_qty as f64,
        sell_quantity: clamp_i32(sell_qty),
        sell_value: sell_price * sell_qty as f64,
    }
}

/// Add a live price and the open quantity's P&L to an open position (web
/// `_attach_ltp`). A missing price leaves LTP 0 and P&L as realised.
pub fn attach_ltp(p: &mut Position, ltp: Option<f64>) {
    let Some(ltp) = ltp.filter(|v| *v > 0.0) else {
        return;
    };
    if p.quantity == 0 {
        return;
    }
    p.ltp = ltp;
    if p.average_price > 0.0 {
        p.unrealized_pnl = (ltp - p.average_price) * f64::from(p.quantity);
        p.pnl = p.realized_pnl + p.unrealized_pnl;
    }
}

/// Phrases a broker uses for "no positions" (web `utils/position_read.py`
/// `_EMPTY_BOOK_MARKERS`). A bare "not found" is not one of them: a
/// refusal that names something else missing is a failed read.
const EMPTY_BOOK_MARKERS: &[&str] = &[
    "no data",
    "nodata",
    "no_data",
    "no-data",
    "no position",
    "no open position",
    "no record",
    "have any position",
    "have any open position",
    "data not found",
    "record not found",
    "positions not found",
];

/// A one-line "no data" message is short; a longer text (an error page)
/// is never read as one (web `_EMPTY_BOOK_MAX_CHARS`).
const EMPTY_BOOK_MAX_CHARS: usize = 2000;

/// Whether a refused read only says the book is empty (web
/// `says_no_positions`). Anything else is a failed read, never an empty
/// book: a smart order must not take a refusal for a flat position.
pub fn says_no_positions(message: &str) -> bool {
    if message.chars().count() > EMPTY_BOOK_MAX_CHARS {
        return false;
    }
    let m = message.to_lowercase();
    EMPTY_BOOK_MARKERS.iter().any(|k| m.contains(*k))
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

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// The exchange a holding is on: where the master contract lists the
/// symbol, NSE first, else BSE; empty when neither (web `get_holdings`;
/// Groww's holdings carry no exchange).
pub fn holding_exchange(h: &GrowwHolding, symbols: &SymbolResolver) -> Option<&'static str> {
    ["NSE", "BSE"]
        .into_iter()
        .find(|ex| symbols.oa_symbol(&h.trading_symbol, ex).is_some())
}

/// One holding (web `get_holdings` + `transform_holdings_data`). Groww's
/// holdings carry no price: `ltp` is the live price when Groww priced the
/// holding, and only then are P&L and P&L % set; an unpriced holding is
/// valued at its average price.
pub fn map_holding(h: &GrowwHolding, symbols: &SymbolResolver, ltp: Option<f64>) -> Holding {
    let exchange = holding_exchange(h, symbols);
    let symbol = exchange
        .and_then(|ex| symbols.oa_symbol(&h.trading_symbol, ex))
        .unwrap_or_else(|| h.trading_symbol.clone());
    let qty = h.quantity as f64;
    let ltp = ltp.filter(|v| *v > 0.0);
    let (pnl, pct) = match ltp {
        Some(l) => (
            round2((l - h.average_price) * qty),
            if h.average_price != 0.0 {
                round2((l - h.average_price) / h.average_price * 100.0)
            } else {
                0.0
            },
        ),
        None => (0.0, 0.0),
    };
    Holding {
        symbol,
        exchange: exchange.unwrap_or("").into(),
        product: "CNC".into(),
        isin: (!h.isin.is_empty()).then(|| h.isin.clone()),
        quantity: clamp_i32(h.quantity),
        t1_quantity: clamp_i32(h.t1_quantity),
        average_price: h.average_price,
        ltp: ltp.map(round2).unwrap_or(0.0),
        close_price: 0.0,
        pnl,
        pnl_percentage: pct,
        current_value: ltp.unwrap_or(h.average_price) * qty,
    }
}

/// Web `calculate_portfolio_statistics`: invested at the average price,
/// held at the live price when Groww priced the holding, else at the
/// average.
pub fn holdings_stats(h: &[Holding]) -> PortfolioStats {
    let inv: f64 = h
        .iter()
        .map(|x| x.average_price * f64::from(x.quantity))
        .sum();
    let value: f64 = h.iter().map(|x| x.current_value).sum();
    let pnl: f64 = h.iter().map(|x| x.pnl).sum();
    PortfolioStats {
        totalholdingvalue: round2(value),
        totalinvvalue: round2(inv),
        totalprofitandloss: round2(pnl),
        totalpnlpercentage: if inv != 0.0 {
            round2(pnl / inv * 100.0)
        } else {
            0.0
        },
    }
}

/// The position segment that holds an exchange's positions; `None` for an
/// exchange the position read does not cover.
pub fn segment_of(exchange: Exchange) -> Option<&'static str> {
    match exchange {
        Exchange::Nse | Exchange::Bse => Some(SEGMENT_CASH),
        Exchange::Nfo | Exchange::Bfo => Some(SEGMENT_FNO),
        _ => None,
    }
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
