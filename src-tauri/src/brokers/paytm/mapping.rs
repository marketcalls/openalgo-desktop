//! OpenAlgo <-> Paytm Money vocabulary and book normalisers (web
//! `mapping/transform_data.py`, `mapping/order_data.py`).
//!
//! Paytm reports every instrument on its parent exchange (`NSE`/`BSE`) with
//! an `instrument` type; derivatives (`OPTIDX`, `FUTSTK`, ...) are moved to
//! `NFO`/`BFO` before the `security_id` is looked up in the master, so books
//! carry OpenAlgo symbols.

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// `{"status": "success"|"error", "message", "data", "errors": [{message}]}`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaytmEnvelope {
    #[serde(deserialize_with = "string_lenient")]
    pub status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub message: String,
    pub data: Value,
    pub errors: Value,
}

impl PaytmEnvelope {
    pub fn is_success(&self) -> bool {
        self.status.eq_ignore_ascii_case("success")
    }

    /// The broker's own explanation (`message`, else the joined `errors`).
    pub fn error_message(&self) -> String {
        if let Some(list) = self.errors.as_array() {
            let joined: Vec<&str> = list
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .filter(|m| !m.trim().is_empty())
                .collect();
            if !joined.is_empty() {
                return joined.join("; ");
            }
        }
        self.message.trim().to_string()
    }

    /// `data` as a list of rows (null or an object yields what it can).
    pub fn rows<T: for<'de> Deserialize<'de>>(&self) -> Vec<T> {
        rows_of(&self.data)
    }
}

/// Deserialise each element of a JSON array; malformed rows are skipped.
pub fn rows_of<T: for<'de> Deserialize<'de>>(v: &Value) -> Vec<T> {
    match v {
        Value::Array(a) => a
            .iter()
            .filter_map(|r| serde_json::from_value(r.clone()).ok())
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// OpenAlgo exchange -> Paytm order exchange (web `map_exchange`).
pub fn paytm_exchange(exchange: Exchange) -> &'static str {
    match exchange {
        Exchange::Nse | Exchange::Nfo | Exchange::NseIndex => "NSE",
        Exchange::Bse | Exchange::Bfo | Exchange::BseIndex => "BSE",
        _ => "EXCHANGE",
    }
}

/// `E` (equity cash) for NSE/BSE, `D` (derivatives) otherwise.
pub fn segment(exchange: Exchange) -> &'static str {
    if exchange.is_cash() {
        "E"
    } else {
        "D"
    }
}

/// OpenAlgo price type -> Paytm `order_type`.
pub fn paytm_order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MKT",
        PriceType::Limit => "LMT",
        PriceType::Sl => "SL",
        PriceType::SlM => "SLM",
    }
}

/// Paytm `order_type` -> OpenAlgo price type.
pub fn oa_order_type(t: &str) -> Option<&'static str> {
    Some(match t.trim().to_ascii_uppercase().as_str() {
        "MKT" | "MARKET" => "MARKET",
        "LMT" | "LIMIT" => "LIMIT",
        "SL" | "STOP_LOSS" | "SL-L" => "SL",
        "SLM" | "SL-M" | "STOP_LOSS_MARKET" => "SL-M",
        _ => return None,
    })
}

/// OpenAlgo product -> Paytm product (`CNC->C, NRML->M, MIS->I`).
pub fn paytm_product(p: Product) -> &'static str {
    match p {
        Product::Cnc => "C",
        Product::Nrml => "M",
        Product::Mis => "I",
    }
}

/// Paytm product -> OpenAlgo product.
pub fn oa_product(p: &str) -> String {
    match p.trim() {
        "C" => "CNC".into(),
        "I" => "MIS".into(),
        "M" => "NRML".into(),
        other => other.to_string(),
    }
}

/// `B`/`S` -> `BUY`/`SELL`.
pub fn oa_side(t: &str) -> String {
    match t.trim() {
        "B" => "BUY".into(),
        "S" => "SELL".into(),
        other => other.to_ascii_uppercase(),
    }
}

pub fn paytm_side(a: Action) -> &'static str {
    match a {
        Action::Buy => "B",
        Action::Sell => "S",
    }
}

/// Order `display_status` -> OpenAlgo status (web `transform_order_data`).
/// Paytm's `Pending` is reported as `trigger pending`, as on the web.
pub fn map_status(display_status: &str) -> String {
    match display_status.trim() {
        "Successful" => "complete".into(),
        "Rejected" => "rejected".into(),
        "Pending" => "trigger pending".into(),
        "Open" => "open".into(),
        "Cancelled" => "cancelled".into(),
        other => crate::brokers::lower_status(other),
    }
}

/// Whether an instrument type marks a derivative.
fn is_derivative(instrument: &str) -> bool {
    instrument.contains("OPT") || instrument.contains("FUT")
}

/// Paytm parent exchange + instrument -> OpenAlgo exchange.
pub fn oa_exchange(exchange: &str, instrument: &str) -> String {
    match exchange.trim() {
        "NSE" if is_derivative(instrument) => "NFO".into(),
        "BSE" if is_derivative(instrument) => "BFO".into(),
        other => other.to_string(),
    }
}

/// OpenAlgo symbol for a `security_id`, falling back to the id itself.
pub fn oa_symbol(symbols: &SymbolResolver, security_id: &str, exchange: &str) -> String {
    symbols
        .by_token(exchange, security_id)
        .map(|r| r.symbol)
        .unwrap_or_else(|| security_id.to_string())
}

/// Paytm `scripType` / pref type for an instrument (web
/// `_prepare_symbol_for_api` / `_determine_scrip_type`).
pub fn scrip_type(row: &SymToken, allow_etf: bool) -> &'static str {
    if matches!(row.exchange.as_str(), "NSE_INDEX" | "BSE_INDEX") {
        return "INDEX";
    }
    match row.instrument_type.as_str() {
        "CE" | "PE" => "OPTION",
        "FUT" => "FUTURE",
        _ if allow_etf && row.symbol.to_ascii_uppercase().contains("ETF") => "ETF",
        _ => "EQUITY",
    }
}

// ---------------------------------------------------------------------------
// Raw rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaytmOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub order_no: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exch_order_no: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub segment: String,
    #[serde(deserialize_with = "string_lenient")]
    pub instrument: String,
    #[serde(deserialize_with = "string_lenient")]
    pub security_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub display_name: String,
    #[serde(deserialize_with = "string_lenient")]
    pub txn_type: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub remaining_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub total_traded_qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub trigger_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub avg_traded_price: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub order_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub display_order_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub validity: String,
    #[serde(deserialize_with = "string_lenient")]
    pub status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub display_status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_date_time: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exch_order_time: String,
    #[serde(deserialize_with = "string_lenient")]
    pub reason_description: String,
    #[serde(deserialize_with = "string_lenient")]
    pub off_mkt_flag: String,
    #[serde(deserialize_with = "string_lenient")]
    pub mkt_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub serial_no: String,
    #[serde(deserialize_with = "string_lenient")]
    pub group_id: String,
    /// The untouched row, for cancel/modify bodies that echo it back.
    #[serde(skip)]
    pub raw: Value,
}

/// Order rows with the raw JSON kept alongside.
pub fn parse_orders(data: &Value) -> Vec<PaytmOrder> {
    match data {
        Value::Array(a) => a
            .iter()
            .filter_map(|r| {
                let mut o: PaytmOrder = serde_json::from_value(r.clone()).ok()?;
                o.raw = r.clone();
                Some(o)
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaytmPosition {
    #[serde(deserialize_with = "string_lenient")]
    pub security_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub segment: String,
    #[serde(deserialize_with = "string_lenient")]
    pub instrument: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub display_name: String,
    #[serde(deserialize_with = "string_lenient")]
    pub display_pos_type: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub net_qty: i64,
    #[serde(rename = "netQty", deserialize_with = "i64_lenient")]
    pub net_qty_alt: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub tot_buy_qty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub tot_sell_qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub tot_buy_val: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub tot_sell_val: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub cf_buy_qty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub cf_sell_qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub buy_avg: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub sell_avg: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub net_avg: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_traded_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub net_val: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub realised_profit: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub unrealised_profit: f64,
}

impl PaytmPosition {
    /// `net_qty`, or `netQty` on rows that spell it that way.
    pub fn quantity(&self) -> i64 {
        if self.net_qty != 0 {
            self.net_qty
        } else {
            self.net_qty_alt
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaytmHolding {
    #[serde(deserialize_with = "string_lenient")]
    pub nse_security_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub bse_security_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub nse_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub bse_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub t1_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub cost_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_traded_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pc: f64,
}

// ---------------------------------------------------------------------------
// Normalisers
// ---------------------------------------------------------------------------

fn clamp_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// web `map_order_data` + `transform_order_data`.
pub fn map_orders(rows: &[PaytmOrder], symbols: &SymbolResolver) -> Vec<Order> {
    rows.iter()
        .map(|o| {
            let exchange = oa_exchange(&o.exchange, &o.instrument);
            let filled = if o.total_traded_qty > 0 {
                o.total_traded_qty
            } else {
                (o.quantity - o.remaining_quantity).max(0)
            };
            let status = map_status(&o.display_status);
            Order {
                order_tag: None,
                order_id: o.order_no.clone(),
                exchange_order_id: non_empty(&o.exch_order_no),
                symbol: oa_symbol(symbols, &o.security_id, &exchange),
                exchange,
                side: oa_side(&o.txn_type),
                quantity: clamp_i32(o.quantity),
                filled_quantity: clamp_i32(filled),
                pending_quantity: clamp_i32(o.remaining_quantity),
                price: o.price,
                trigger_price: o.trigger_price,
                average_price: o.avg_traded_price,
                order_type: oa_order_type(&o.order_type)
                    .map(str::to_string)
                    .unwrap_or_else(|| o.display_order_type.to_ascii_uppercase()),
                product: oa_product(&o.product),
                validity: if o.validity.is_empty() {
                    "DAY".into()
                } else {
                    o.validity.clone()
                },
                order_timestamp: o.order_date_time.clone(),
                exchange_timestamp: non_empty(&o.exch_order_time),
                rejection_reason: if status == "rejected" {
                    non_empty(&o.reason_description)
                } else {
                    None
                },
                status,
            }
        })
        .collect()
}

/// web `transform_tradebook_data` over the order list (Paytm has no trade
/// book). Only orders that traded are reported, valued at the traded
/// quantity times the average traded price.
pub fn map_trades(rows: &[PaytmOrder], symbols: &SymbolResolver) -> Vec<Trade> {
    rows.iter()
        .filter_map(|o| {
            let qty = if o.total_traded_qty > 0 {
                o.total_traded_qty
            } else if o.display_status == "Successful" {
                o.quantity
            } else {
                0
            };
            if qty <= 0 || o.avg_traded_price <= 0.0 {
                return None;
            }
            let exchange = oa_exchange(&o.exchange, &o.instrument);
            Some(Trade {
                order_tag: None,
                order_id: o.order_no.clone(),
                trade_id: o.order_no.clone(),
                symbol: oa_symbol(symbols, &o.security_id, &exchange),
                exchange,
                product: oa_product(&o.product),
                side: oa_side(&o.txn_type),
                quantity: clamp_i32(qty),
                average_price: o.avg_traded_price,
                trade_value: round2(qty as f64 * o.avg_traded_price),
                timestamp: if o.exch_order_time.is_empty() {
                    o.order_date_time.clone()
                } else {
                    o.exch_order_time.clone()
                },
            })
        })
        .collect()
}

/// web `map_position_data` + `transform_positions_data`.
pub fn map_positions(rows: &[PaytmPosition], symbols: &SymbolResolver) -> Vec<Position> {
    rows.iter()
        .map(|p| {
            let exchange = oa_exchange(&p.exchange, &p.instrument);
            let average = match p.display_pos_type.as_str() {
                "B" => p.buy_avg,
                "S" => p.sell_avg,
                _ => p.net_avg,
            };
            Position {
                symbol: oa_symbol(symbols, &p.security_id, &exchange),
                exchange,
                product: oa_product(&p.product),
                quantity: clamp_i32(p.quantity()),
                overnight_quantity: clamp_i32(p.cf_buy_qty - p.cf_sell_qty),
                average_price: round2(average),
                ltp: p.last_traded_price,
                pnl: p.net_val,
                realized_pnl: p.realised_profit,
                unrealized_pnl: p.unrealised_profit,
                buy_quantity: clamp_i32(p.tot_buy_qty),
                buy_value: p.tot_buy_val,
                sell_quantity: clamp_i32(p.tot_sell_qty),
                sell_value: p.tot_sell_val,
            }
        })
        .collect()
}

/// Holdings rows from `data` (a list, or `{"results": [...]}`).
pub fn holding_rows(data: &Value) -> Vec<PaytmHolding> {
    match data {
        Value::Object(o) => o.get("results").map(rows_of).unwrap_or_default(),
        other => rows_of(other),
    }
}

/// web `map_portfolio_data` + `transform_holdings_data`: NSE unless the row
/// is BSE-only or says `BSE`.
pub fn map_holdings(rows: &[PaytmHolding], symbols: &SymbolResolver) -> Vec<Holding> {
    rows.iter()
        .map(|h| {
            let bse = (h.nse_security_id.is_empty() && !h.bse_security_id.is_empty())
                || h.exchange == "BSE";
            let (exchange, id, br) = if bse {
                ("BSE", &h.bse_security_id, &h.bse_symbol)
            } else {
                ("NSE", &h.nse_security_id, &h.nse_symbol)
            };
            let symbol = (!id.is_empty())
                .then(|| symbols.by_token(exchange, id).map(|r| r.symbol))
                .flatten()
                .or_else(|| non_empty(br))
                .unwrap_or_else(|| "Unknown".to_string());
            let qty = h.quantity as f64;
            let pnl = round2((h.last_traded_price - h.cost_price) * qty);
            Holding {
                symbol,
                exchange: exchange.to_string(),
                product: "CNC".into(),
                isin: non_empty(&h.isin),
                quantity: clamp_i32(h.quantity),
                t1_quantity: clamp_i32(h.t1_quantity),
                average_price: h.cost_price,
                ltp: h.last_traded_price,
                close_price: h.pc,
                pnl,
                pnl_percentage: if h.cost_price > 0.0 {
                    round2((h.last_traded_price - h.cost_price) / h.cost_price * 100.0)
                } else {
                    0.0
                },
                current_value: round2(h.last_traded_price * qty),
            }
        })
        .collect()
}
