//! OpenAlgo <-> Arrow vocabularies and book normalisers (web
//! `mapping/transform_data.py`, `mapping/order_data.py`,
//! `mapping/exchange.py`).

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::lower_status;
use crate::brokers::types::*;
use serde::Deserialize;

/// OpenAlgo price type -> Arrow `order` (`transform_data.py:16-21`).
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MKT",
        PriceType::Limit => "LMT",
        PriceType::Sl => "SL-LMT",
        PriceType::SlM => "SL-MKT",
    }
}

/// Arrow `order` -> OpenAlgo price type (`order_data.py:9-14`; the order
/// stream also sends `SL` / `SL-M`, `arrow_order_adapter.py:42`).
pub fn price_type_from_arrow(s: &str) -> String {
    match s {
        "LMT" => "LIMIT",
        "MKT" => "MARKET",
        "SL-LMT" | "SL" => "SL",
        "SL-MKT" | "SL-M" => "SL-M",
        other => other,
    }
    .to_string()
}

/// OpenAlgo product -> Arrow product (`transform_data.py:26-30`).
pub fn product_code(p: Product) -> &'static str {
    match p {
        Product::Cnc => "C",
        Product::Nrml => "M",
        Product::Mis => "I",
    }
}

/// Arrow product -> OpenAlgo product (`transform_data.py:33-37`); unknown
/// codes pass through.
pub fn product_from_arrow(s: &str) -> String {
    match s {
        "C" => "CNC",
        "M" => "NRML",
        "I" => "MIS",
        other => other,
    }
    .to_string()
}

pub fn side_code(a: Action) -> &'static str {
    match a {
        Action::Buy => "B",
        Action::Sell => "S",
    }
}

pub fn side_from_arrow(s: &str) -> String {
    match s {
        "B" => "BUY",
        "S" => "SELL",
        other => other,
    }
    .to_string()
}

/// Arrow `orderStatus` -> OpenAlgo status (`order_data.py:16-23` plus
/// `AFTER_MARKET_ORDER_REQ_RECEIVED` from `arrow_order_adapter.py:31-39`);
/// anything else lowercased.
pub fn map_status(s: &str) -> String {
    match s {
        "COMPLETE" => "complete".into(),
        "OPEN" | "PENDING" | "AFTER_MARKET_ORDER_REQ_RECEIVED" => "open".into(),
        "TRIGGER_PENDING" => "trigger pending".into(),
        "CANCELLED" => "cancelled".into(),
        "REJECTED" => "rejected".into(),
        other => lower_status(other),
    }
}

/// Statuses cancel-all touches (`order_api.py:364-375`).
pub fn is_cancellable(s: &str) -> bool {
    matches!(s, "OPEN" | "PENDING" | "TRIGGER_PENDING")
}

/// Exchanges Arrow's quote REST API does not serve (`exchange.py:59`); one
/// such leg would 400 a whole batch.
pub fn quote_unsupported(exchange: &str) -> bool {
    matches!(exchange, "CDS" | "BCD" | "NCO")
}

/// OpenAlgo exchange -> Arrow quote exchange (`exchange.py:43-52`): MCX is
/// `MCXFO`, every index exchange is `INDEX`.
pub fn quote_exchange(exchange: &str) -> String {
    match exchange {
        "MCX" => "MCXFO".into(),
        "NSE_INDEX" | "BSE_INDEX" | "MCX_INDEX" => "INDEX".into(),
        other => other.to_string(),
    }
}

/// OpenAlgo exchange -> history path segment (`exchange.py:65-77`).
pub fn history_exchange(exchange: &str) -> String {
    match exchange {
        "NSE_INDEX" => "nse".into(),
        "BSE_INDEX" => "bse".into(),
        "MCX_INDEX" => "mcx".into(),
        other => other.to_ascii_lowercase(),
    }
}

// ---------------------------------------------------------------------------
// Raw rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArrowOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub order_no: String,
    #[serde(deserialize_with = "string_lenient")]
    pub id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange_order_no: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub cumulative_fill_qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub trigger_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub order: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub validity: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_time: String,
    #[serde(deserialize_with = "string_lenient")]
    pub request_time: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange_update_time: String,
    #[serde(deserialize_with = "string_lenient")]
    pub rejection_reason: String,
}

impl ArrowOrder {
    /// `orderNo`, else `id` (`order_api.py:369-372`).
    pub fn order_id(&self) -> &str {
        if self.order_no.is_empty() {
            &self.id
        } else {
            &self.order_no
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArrowTrade {
    #[serde(deserialize_with = "string_lenient")]
    pub order_no: String,
    #[serde(deserialize_with = "string_lenient")]
    pub id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub fill_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    pub fill_quantity: Option<serde_json::Value>,
    pub quantity: Option<serde_json::Value>,
    pub fill_price: Option<serde_json::Value>,
    pub average_price: Option<serde_json::Value>,
    #[serde(deserialize_with = "string_lenient")]
    pub fill_time: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_time: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArrowPosition {
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub avg_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ltp: f64,
    #[serde(rename = "realisedPnL", deserialize_with = "f64_lenient")]
    pub realised_pnl: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub unrealised_mark_to_market: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub buy_qty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub sell_qty: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArrowHoldingSymbol {
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArrowHolding {
    pub symbols: Vec<ArrowHoldingSymbol>,
    #[serde(deserialize_with = "i64_lenient")]
    pub qty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub avg_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pnl: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub close: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
}

/// Rows of a `{status, data: [...]}` payload, leniently: a bad row is
/// skipped (and logged) rather than failing the book.
pub fn rows<T: serde::de::DeserializeOwned>(data: serde_json::Value, what: &str) -> Vec<T> {
    match data {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|r| match serde_json::from_value::<T>(r) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("Skipped an Arrow {} row: {}", what, e);
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn i32_of(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn lenient_num(v: &Option<serde_json::Value>) -> Option<f64> {
    match v.as_ref()? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Order book (`map_order_data` + `transform_order_data`).
pub fn map_orders(rows: Vec<ArrowOrder>, symbols: &SymbolResolver) -> Vec<Order> {
    rows.into_iter()
        .map(|o| {
            let status = map_status(&o.order_status);
            let filled = o.cumulative_fill_qty;
            let ts = if o.order_time.is_empty() {
                o.request_time.clone()
            } else {
                o.order_time.clone()
            };
            Order {
                order_id: o.order_id().to_string(),
                exchange_order_id: (!o.exchange_order_no.is_empty())
                    .then(|| o.exchange_order_no.clone()),
                symbol: symbols.oa_symbol_or_raw(&o.symbol, &o.exchange),
                exchange: o.exchange.clone(),
                side: side_from_arrow(&o.transaction_type),
                quantity: i32_of(o.quantity),
                filled_quantity: i32_of(filled),
                pending_quantity: i32_of((o.quantity - filled).max(0)),
                price: o.price,
                trigger_price: o.trigger_price,
                average_price: o.average_price,
                order_type: price_type_from_arrow(&o.order),
                product: product_from_arrow(&o.product),
                rejection_reason: (status == "rejected" && !o.rejection_reason.is_empty())
                    .then(|| o.rejection_reason.clone()),
                status,
                validity: if o.validity.is_empty() {
                    "DAY".into()
                } else {
                    o.validity.clone()
                },
                order_timestamp: ts,
                exchange_timestamp: (!o.exchange_update_time.is_empty())
                    .then(|| o.exchange_update_time.clone()),
            }
        })
        .collect()
}

/// Trade book (`transform_tradebook_data`): `fillQuantity` / `fillPrice` /
/// `fillTime`, falling back to the order-style fields.
pub fn map_trades(rows: Vec<ArrowTrade>, symbols: &SymbolResolver) -> Vec<Trade> {
    rows.into_iter()
        .map(|t| {
            let qty = lenient_num(&t.fill_quantity)
                .or_else(|| lenient_num(&t.quantity))
                .unwrap_or(0.0);
            let avg = lenient_num(&t.fill_price)
                .or_else(|| lenient_num(&t.average_price))
                .unwrap_or(0.0);
            Trade {
                order_id: if t.order_no.is_empty() {
                    t.id.clone()
                } else {
                    t.order_no.clone()
                },
                trade_id: t.fill_id.clone(),
                symbol: symbols.oa_symbol_or_raw(&t.symbol, &t.exchange),
                exchange: t.exchange.clone(),
                product: product_from_arrow(&t.product),
                side: side_from_arrow(&t.transaction_type),
                quantity: i32_of(qty as i64),
                average_price: avg,
                trade_value: qty * avg,
                timestamp: if t.fill_time.is_empty() {
                    t.order_time.clone()
                } else {
                    t.fill_time.clone()
                },
            }
        })
        .collect()
}

/// Net positions (`map_position_data` + `transform_positions_data`).
pub fn map_positions(rows: Vec<ArrowPosition>, symbols: &SymbolResolver) -> Vec<Position> {
    rows.into_iter()
        .map(|p| Position {
            symbol: symbols.oa_symbol_or_raw(&p.symbol, &p.exchange),
            exchange: p.exchange.clone(),
            product: product_from_arrow(&p.product),
            quantity: i32_of(p.qty),
            overnight_quantity: 0,
            average_price: round2(p.avg_price),
            ltp: round2(p.ltp),
            pnl: round2(p.realised_pnl + p.unrealised_mark_to_market),
            realized_pnl: p.realised_pnl,
            unrealized_pnl: p.unrealised_mark_to_market,
            buy_quantity: i32_of(p.buy_qty),
            buy_value: 0.0,
            sell_quantity: i32_of(p.sell_qty),
            sell_value: 0.0,
        })
        .collect()
}

/// Holdings (`map_portfolio_data` + `transform_holdings_data`): each holding
/// lists its instruments under `symbols[]`; the first is used.
pub fn map_holdings(rows: Vec<ArrowHolding>, symbols: &SymbolResolver) -> Vec<Holding> {
    rows.into_iter()
        .map(|h| {
            let primary = h.symbols.first().cloned().unwrap_or_default();
            let br = if primary.trading_symbol.is_empty() {
                primary.symbol.clone()
            } else {
                primary.trading_symbol.clone()
            };
            let symbol = if !br.is_empty() && !primary.exchange.is_empty() {
                symbols.oa_symbol_or_raw(&br, &primary.exchange)
            } else {
                br
            };
            let pnl_percentage = if h.avg_price == 0.0 {
                0.0
            } else {
                round2((h.ltp - h.avg_price) / h.avg_price * 100.0)
            };
            let isin = if primary.isin.is_empty() {
                h.isin.clone()
            } else {
                primary.isin.clone()
            };
            Holding {
                symbol,
                exchange: primary.exchange.clone(),
                product: Product::Cnc.as_str().into(),
                isin: (!isin.is_empty()).then_some(isin),
                quantity: i32_of(h.qty),
                t1_quantity: 0,
                average_price: h.avg_price,
                ltp: h.ltp,
                close_price: h.close,
                pnl: round2(h.pnl),
                pnl_percentage,
                current_value: h.ltp * h.qty as f64,
            }
        })
        .collect()
}

/// Whether an OpenAlgo exchange is one of the quote-only index exchanges.
pub fn is_index(exchange: &str) -> bool {
    exchange
        .parse::<Exchange>()
        .map(Exchange::is_index)
        .unwrap_or(false)
}
