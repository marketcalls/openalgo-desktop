//! Kite <-> OpenAlgo translation (web `broker/zerodha/mapping/*`).
//!
//! Kite payload structs are null-tolerant; every book normaliser rewrites
//! the Kite tradingsymbol to the OpenAlgo symbol and converts MCX
//! quantities from Kite contracts to OpenAlgo units (one CRUDEOIL lot is
//! 100 units on every broker; Kite counts it as 1 contract).

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::NaiveDate;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// MCX contract sizes (web mapping/mcx_contract_size.py)
// ---------------------------------------------------------------------------

/// Underlying root -> units in one contract.
pub const MCX_CONTRACT_SIZES: &[(&str, i64)] = &[
    ("ALUMINI", 1),
    ("ALUMINIUM", 5),
    ("CARDAMOM", 100),
    ("COPPER", 2500),
    ("COTTON", 25),
    ("COTTONOIL", 5),
    ("CRUDEOIL", 100),
    ("CRUDEOILM", 10),
    ("ELECDMBL", 50),
    ("GOLD", 1),
    ("GOLDGUINEA", 8),
    ("GOLDM", 100),
    ("GOLDPETAL", 1),
    ("GOLDTEN", 10),
    ("KAPAS", 4),
    ("LEAD", 5),
    ("LEADMINI", 1),
    ("MCXBULLDEX", 30),
    ("MCXMETLDEX", 40),
    ("MENTHAOIL", 360),
    ("NATGASMINI", 250),
    ("NATURALGAS", 1250),
    ("NICKEL", 250),
    ("SILVER", 30),
    ("SILVER100", 100),
    ("SILVERM", 5),
    ("SILVERMIC", 1),
    ("STEELREBAR", 5),
    ("ZINC", 5),
    ("ZINCMINI", 1),
];

/// Roots whose size MCX revised: (first expiry on the new size, new size).
pub const MCX_SIZE_REVISIONS: &[(&str, (i32, u32, u32), i64)] =
    &[("MCXBULLDEX", (2026, 11, 1), 15)];

/// Root -> quotation multiplier where it differs from the contract size.
/// Display valuation only (trade value); never sizes an order.
pub const MCX_QUOTATION_MULTIPLIERS: &[(&str, i64)] = &[
    ("GOLD", 100),
    ("GOLDM", 10),
    ("GOLDGUINEA", 1),
    ("GOLDTEN", 1),
    ("SILVER100", 10),
    ("ZINC", 5000),
    ("ZINCMINI", 1000),
    ("LEAD", 5000),
    ("LEADMINI", 1000),
    ("ALUMINIUM", 5000),
    ("ALUMINI", 1000),
    ("KAPAS", 200),
    ("COTTONOIL", 500),
];

fn is_mcx(exchange: &str) -> bool {
    exchange.trim().eq_ignore_ascii_case("MCX")
}

/// The MCX underlying a symbol (OpenAlgo or Kite form) belongs to, longest
/// root first so `GOLDPETAL` is not read as `GOLD`.
pub fn mcx_root(symbol: &str) -> Option<&'static str> {
    let s = symbol.trim().to_ascii_uppercase();
    MCX_CONTRACT_SIZES
        .iter()
        .filter(|(root, _)| s.starts_with(root))
        .max_by_key(|(root, _)| root.len())
        .map(|(root, _)| *root)
}

/// Contract size for an exact underlying `name` and expiry (master contract).
pub fn contract_size(name: &str, expiry: Option<NaiveDate>) -> Option<i64> {
    let root = name.trim().to_ascii_uppercase();
    let mut size = MCX_CONTRACT_SIZES
        .iter()
        .find(|(r, _)| *r == root)
        .map(|(_, s)| *s)?;
    if let Some(exp) = expiry {
        for (r, (y, m, d), revised) in MCX_SIZE_REVISIONS {
            if *r == root {
                if let Some(from) = NaiveDate::from_ymd_opt(*y, *m, *d) {
                    if exp >= from {
                        size = *revised;
                    }
                }
            }
        }
    }
    Some(size)
}

/// Units per Kite contract. `master_lot` is the master-contract lot size for
/// this exact contract (authoritative). `None` only when the size cannot be
/// established (a revised root with no master row).
pub fn units_per_contract(symbol: &str, exchange: &str, master_lot: Option<i64>) -> Option<i64> {
    if !is_mcx(exchange) || symbol.is_empty() {
        return Some(1);
    }
    if let Some(l) = master_lot.filter(|l| *l > 0) {
        return Some(l);
    }
    match mcx_root(symbol) {
        Some(root) if MCX_SIZE_REVISIONS.iter().any(|(r, _, _)| *r == root) => None,
        Some(root) => contract_size(root, None),
        None => Some(1),
    }
}

/// OpenAlgo units -> Kite contracts (outbound). Refuses quantities that are
/// not whole contracts rather than rounding them.
pub fn to_kite_quantity(
    qty: i64,
    symbol: &str,
    exchange: &str,
    master_lot: Option<i64>,
    field: &str,
) -> Result<i64> {
    let size = units_per_contract(symbol, exchange, master_lot).ok_or_else(|| {
        AppError::Validation(format!(
            "Cannot size {}: MCX has revised its contract size and the master contract has no row for this expiry. Re-download the master contract, then retry.",
            symbol
        ))
    })?;
    if size == 1 {
        return Ok(qty);
    }
    let (contracts, rem) = (qty.abs() / size, qty.abs() % size);
    if rem != 0 {
        return Err(AppError::Validation(format!(
            "{} must be in multiples of lot size {} for {}, got {}",
            field,
            size,
            symbol,
            qty.abs()
        )));
    }
    Ok(if qty < 0 { -contracts } else { contracts })
}

/// Kite contracts -> OpenAlgo units (inbound). Unknown size passes through.
pub fn from_kite_quantity(qty: i64, symbol: &str, exchange: &str, master_lot: Option<i64>) -> i64 {
    match units_per_contract(symbol, exchange, master_lot) {
        Some(size) if size != 1 => qty * size,
        _ => qty,
    }
}

/// Rupee value of one contract per unit of price (display only).
pub fn price_multiplier(symbol: &str, exchange: &str, master_lot: Option<i64>) -> i64 {
    if !is_mcx(exchange) {
        return 1;
    }
    match mcx_root(symbol) {
        Some(root) => MCX_QUOTATION_MULTIPLIERS
            .iter()
            .find(|(r, _)| *r == root)
            .map(|(_, m)| *m)
            .unwrap_or_else(|| units_per_contract(symbol, exchange, master_lot).unwrap_or(1)),
        None => 1,
    }
}

/// Master-contract lot size for a Kite tradingsymbol on MCX.
fn master_lot(symbols: &SymbolResolver, brsymbol: &str, exchange: &str) -> Option<i64> {
    if !is_mcx(exchange) {
        return None;
    }
    symbols
        .by_brsymbol(exchange, brsymbol)
        .map(|r| i64::from(r.lot_size))
}

// ---------------------------------------------------------------------------
// Enum maps (web mapping/transform_data.py)
// ---------------------------------------------------------------------------

/// Kite order status -> OpenAlgo lowercase status. In-flight OMS states are
/// still working orders, so they read as `open` (zerodha_order_adapter.py).
pub fn map_status(kite: &str) -> String {
    match kite.trim() {
        "COMPLETE" => "complete".into(),
        "REJECTED" => "rejected".into(),
        "CANCELLED" => "cancelled".into(),
        "TRIGGER PENDING" => "trigger pending".into(),
        "OPEN"
        | "UPDATE"
        | "VALIDATION PENDING"
        | "PUT ORDER REQ RECEIVED"
        | "OPEN PENDING"
        | "MODIFY VALIDATION PENDING"
        | "MODIFY PENDING"
        | "CANCEL PENDING"
        | "AMO REQ RECEIVED"
        | "TRIGGER PENDING REQ RECEIVED" => "open".into(),
        other => crate::brokers::lower_status(other),
    }
}

/// Status shown in the REST order book. Kite's trigger-pending stop orders
/// are live and can be modified or cancelled, so the book presents them as
/// `open`, its actionable working state; live order updates keep the detailed
/// `trigger pending` from [`map_status`] (web #2185, `transform_order_data`).
pub fn book_status(kite: &str) -> String {
    match map_status(kite).as_str() {
        "trigger pending" => "open".into(),
        other => other.into(),
    }
}

/// Kite exchange prefix for `/quote*` calls (web `_kite_quote_exchange`).
pub fn kite_quote_exchange(oa_exchange: &str, brexchange: &str) -> String {
    match oa_exchange {
        "NSE_INDEX" => "NSE".into(),
        "BSE_INDEX" => "BSE".into(),
        "MCX_INDEX" => "MCX".into(),
        "GLOBAL_INDEX" => {
            if !brexchange.is_empty() && brexchange != "GLOBAL_INDEX" {
                brexchange.into()
            } else {
                "GLOBAL".into()
            }
        }
        other => other.into(),
    }
}

/// `instrument_token::::exchange_token` -> instrument token.
pub fn instrument_token(token: &str) -> Option<u32> {
    token.split("::::").next()?.trim().parse().ok()
}

// ---------------------------------------------------------------------------
// Kite payloads
// ---------------------------------------------------------------------------

/// Kite response envelope.
#[derive(Debug, Deserialize)]
pub struct KiteEnvelope<T> {
    #[serde(default, deserialize_with = "string_lenient")]
    pub status: String,
    pub data: Option<T>,
    #[serde(default, deserialize_with = "string_lenient")]
    pub message: String,
    #[serde(default, deserialize_with = "string_lenient")]
    pub error_type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange_order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub status_message: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_timestamp: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange_timestamp: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub validity: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub trigger_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub filled_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub pending_quantity: i64,
    /// The tag the order was placed with (the reconciler's lookup).
    #[serde(deserialize_with = "string_lenient")]
    pub tag: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteTrade {
    #[serde(deserialize_with = "string_lenient")]
    pub trade_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub fill_timestamp: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_timestamp: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KitePosition {
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub overnight_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub multiplier: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pnl: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub realised: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub unrealised: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub buy_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub buy_value: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub sell_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub sell_value: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KitePositions {
    pub net: Option<Vec<KitePosition>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteHolding {
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub t1_quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub close_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pnl: f64,
}

// ---------------------------------------------------------------------------
// Book normalisers (web mapping/order_data.py)
// ---------------------------------------------------------------------------

fn qty_units(symbols: &SymbolResolver, q: i64, brsymbol: &str, exchange: &str) -> i32 {
    let lot = master_lot(symbols, brsymbol, exchange);
    clamp_i32(from_kite_quantity(q, brsymbol, exchange, lot))
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// `map_order_data` + `transform_order_data`.
pub fn map_orders(rows: Vec<KiteOrder>, symbols: &SymbolResolver) -> Vec<Order> {
    rows.into_iter()
        .map(|o| {
            let units = |q| qty_units(symbols, q, &o.tradingsymbol, &o.exchange);
            Order {
                order_tag: None,
                order_id: o.order_id.clone(),
                exchange_order_id: non_empty(&o.exchange_order_id),
                symbol: symbols.oa_symbol_or_raw(&o.tradingsymbol, &o.exchange),
                exchange: o.exchange.clone(),
                side: o.transaction_type.clone(),
                quantity: units(o.quantity),
                filled_quantity: units(o.filled_quantity),
                pending_quantity: units(o.pending_quantity),
                price: o.price,
                trigger_price: o.trigger_price,
                average_price: o.average_price,
                order_type: o.order_type.clone(),
                product: o.product.clone(),
                status: book_status(&o.status),
                validity: o.validity.clone(),
                order_timestamp: o.order_timestamp.clone(),
                exchange_timestamp: non_empty(&o.exchange_timestamp),
                rejection_reason: if o.status == "REJECTED" {
                    non_empty(&o.status_message)
                } else {
                    None
                },
            }
        })
        .collect()
}

/// `map_trade_data` + `transform_tradebook_data`.
pub fn map_trades(rows: Vec<KiteTrade>, symbols: &SymbolResolver) -> Vec<Trade> {
    rows.into_iter()
        .map(|t| {
            let lot = master_lot(symbols, &t.tradingsymbol, &t.exchange);
            let units = from_kite_quantity(t.quantity, &t.tradingsymbol, &t.exchange, lot);
            // Value the contracts by their quotation multiplier (GOLDGUINEA is
            // 8 g quoted per 8 g; units x price would count it eight times).
            let per = units_per_contract(&t.tradingsymbol, &t.exchange, lot).unwrap_or(1);
            let contracts = units as f64 / per as f64;
            let trade_value = contracts
                * price_multiplier(&t.tradingsymbol, &t.exchange, lot) as f64
                * t.average_price;
            Trade {
                order_tag: None,
                order_id: t.order_id.clone(),
                trade_id: t.trade_id.clone(),
                symbol: symbols.oa_symbol_or_raw(&t.tradingsymbol, &t.exchange),
                exchange: t.exchange.clone(),
                product: t.product.clone(),
                side: t.transaction_type.clone(),
                quantity: clamp_i32(units),
                average_price: t.average_price,
                trade_value,
                timestamp: if t.fill_timestamp.is_empty() {
                    t.order_timestamp.clone()
                } else {
                    t.fill_timestamp.clone()
                },
            }
        })
        .collect()
}

fn round2(v: f64) -> f64 {
    crate::brokers::common::mpp::py_round(v, 2)
}

/// `map_position_data` + `transform_positions_data` (`data.net`).
pub fn map_positions(rows: Vec<KitePosition>, symbols: &SymbolResolver) -> Vec<Position> {
    rows.into_iter()
        .map(|p| {
            let units = |q| qty_units(symbols, q, &p.tradingsymbol, &p.exchange);
            Position {
                symbol: symbols.oa_symbol_or_raw(&p.tradingsymbol, &p.exchange),
                exchange: p.exchange.clone(),
                product: p.product.clone(),
                quantity: units(p.quantity),
                overnight_quantity: units(p.overnight_quantity),
                average_price: round2(p.average_price),
                ltp: round2(p.last_price),
                pnl: round2(p.pnl),
                realized_pnl: p.realised,
                unrealized_pnl: p.unrealised,
                buy_quantity: units(p.buy_quantity),
                buy_value: p.buy_value,
                sell_quantity: units(p.sell_quantity),
                sell_value: p.sell_value,
            }
        })
        .collect()
}

/// `map_portfolio_data` + `transform_holdings_data`. Product is always CNC.
pub fn map_holdings(rows: Vec<KiteHolding>, symbols: &SymbolResolver) -> Vec<Holding> {
    rows.into_iter()
        .map(|h| {
            let pnl_percentage = if h.average_price == 0.0 || h.last_price == 0.0 {
                0.0
            } else {
                round2((h.last_price - h.average_price) / h.average_price * 100.0)
            };
            Holding {
                symbol: symbols.oa_symbol_or_raw(&h.tradingsymbol, &h.exchange),
                exchange: h.exchange.clone(),
                product: "CNC".into(),
                isin: non_empty(&h.isin),
                quantity: clamp_i32(h.quantity),
                t1_quantity: clamp_i32(h.t1_quantity),
                average_price: h.average_price,
                ltp: h.last_price,
                close_price: h.close_price,
                pnl: round2(h.pnl),
                pnl_percentage,
                current_value: h.last_price * h.quantity as f64,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcx_roots_resolve_longest_first() {
        assert_eq!(mcx_root("GOLDPETAL26OCTFUT"), Some("GOLDPETAL"));
        assert_eq!(mcx_root("SILVERMIC26SEPFUT"), Some("SILVERMIC"));
        assert_eq!(mcx_root("CRUDEOILM26SEPFUT"), Some("CRUDEOILM"));
        assert_eq!(mcx_root("CRUDEOIL19OCT26FUT"), Some("CRUDEOIL"));
        assert_eq!(mcx_root("UNKNOWN"), None);
    }

    #[test]
    fn contract_sizes_with_revisions() {
        let oct = NaiveDate::from_ymd_opt(2026, 10, 27);
        let nov = NaiveDate::from_ymd_opt(2026, 11, 24);
        assert_eq!(contract_size("CRUDEOIL", None), Some(100));
        assert_eq!(contract_size("MCXBULLDEX", oct), Some(30));
        assert_eq!(contract_size("MCXBULLDEX", nov), Some(15));
        assert_eq!(contract_size("PEPPER", None), None);
    }

    #[test]
    fn mcx_quantity_conversion() {
        assert_eq!(
            to_kite_quantity(100, "CRUDEOIL19OCT26FUT", "MCX", Some(100), "Quantity").unwrap(),
            1
        );
        assert_eq!(
            to_kite_quantity(-200, "CRUDEOIL19OCT26FUT", "MCX", None, "Quantity").unwrap(),
            -2
        );
        let e =
            to_kite_quantity(150, "CRUDEOIL19OCT26FUT", "MCX", Some(100), "Quantity").unwrap_err();
        assert_eq!(
            e.client_message(),
            "Quantity must be in multiples of lot size 100 for CRUDEOIL19OCT26FUT, got 150"
        );
        // A revised root with no master row refuses rather than guessing.
        assert!(to_kite_quantity(30, "MCXBULLDEX26NOVFUT", "MCX", None, "Quantity").is_err());
        // Off MCX it is a no-op.
        assert_eq!(
            to_kite_quantity(75, "NIFTY27OCT26FUT", "NFO", None, "Quantity").unwrap(),
            75
        );
        assert_eq!(from_kite_quantity(2, "CRUDEOIL26OCTFUT", "MCX", None), 200);
        assert_eq!(from_kite_quantity(2, "MCXBULLDEX26NOVFUT", "MCX", None), 2);
        assert_eq!(price_multiplier("GOLD26DECFUT", "MCX", None), 100);
        assert_eq!(price_multiplier("CRUDEOIL26OCTFUT", "MCX", None), 100);
        assert_eq!(price_multiplier("SBIN", "NSE", None), 1);
    }

    #[test]
    fn statuses() {
        assert_eq!(map_status("COMPLETE"), "complete");
        assert_eq!(map_status("TRIGGER PENDING"), "trigger pending");
        assert_eq!(map_status("PUT ORDER REQ RECEIVED"), "open");
        assert_eq!(map_status("AMO REQ RECEIVED"), "open");
        assert_eq!(map_status("CANCELLED AMO"), "cancelled amo");
        // The REST book shows trigger-pending as open; updates keep it.
        assert_eq!(book_status("TRIGGER PENDING"), "open");
        assert_eq!(book_status("complete"), "complete");
        // Each order maps on its own status: no state carried between rows.
        let rows: Vec<String> = ["COMPLETE", "VALIDATION PENDING", "REJECTED", "OPEN PENDING"]
            .iter()
            .map(|s| book_status(s))
            .collect();
        assert_eq!(rows, ["complete", "open", "rejected", "open"]);
    }

    #[test]
    fn quote_exchange_prefix() {
        assert_eq!(kite_quote_exchange("NSE_INDEX", "NSE"), "NSE");
        assert_eq!(kite_quote_exchange("MCX_INDEX", "MCX"), "MCX");
        assert_eq!(kite_quote_exchange("GLOBAL_INDEX", "NSEIX"), "NSEIX");
        assert_eq!(
            kite_quote_exchange("GLOBAL_INDEX", "GLOBAL_INDEX"),
            "GLOBAL"
        );
        assert_eq!(kite_quote_exchange("NFO", "NFO"), "NFO");
        assert_eq!(instrument_token("408065::::1594"), Some(408065));
        assert_eq!(instrument_token("x"), None);
    }
}
