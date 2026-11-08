//! Vortex books -> OpenAlgo rows (web `mapping/order_data.py`).
//!
//! Rows carry a ticker (`NSE:NIFTY26OCT25000CE`) and a Vortex exchange
//! (`NSE_FO`); the OpenAlgo symbol is the master row whose `brsymbol` is the
//! ticker's symbol part. MCX quantities come back in lots and leave in
//! units.

use super::mapping::{
    from_vortex_quantity, map_status, oa_exchange, price_type_from_variety, product_from_vortex,
    split_ticker,
};
use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::brokers::zerodha::mapping::MCX_QUOTATION_MULTIPLIERS;
use serde::Deserialize;

fn clamp_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// What an order, trade or position row resolves to.
struct RowKey {
    symbol: String,
    exchange: String,
    vortex_exchange: String,
    product: String,
    /// For the MCX lot/unit conversion: the row's own, else the master's.
    lot_size: i64,
    /// The master's underlying name (MCX trade value).
    underlying: Option<String>,
}

fn row_key(
    symbols: &SymbolResolver,
    ticker: &str,
    symbol: &str,
    vortex_exchange: &str,
    product: &str,
    row_lot: i64,
) -> RowKey {
    let exchange = oa_exchange(vortex_exchange);
    let brsymbol = match split_ticker(ticker) {
        "" => symbol,
        s => s,
    };
    let row: Option<SymToken> = symbols.by_brsymbol(&exchange, brsymbol);
    let master_lot = row
        .as_ref()
        .map(|r| i64::from(r.lot_size))
        .filter(|l| *l > 0);
    let product = match product_from_vortex(&exchange, product) {
        p if p.is_empty() => product.to_string(),
        p => p,
    };
    RowKey {
        symbol: row
            .as_ref()
            .map(|r| r.symbol.clone())
            .unwrap_or_else(|| brsymbol.to_string()),
        vortex_exchange: vortex_exchange.to_string(),
        product,
        lot_size: if row_lot > 0 {
            row_lot
        } else {
            master_lot.unwrap_or(1)
        },
        underlying: row.map(|r| r.name),
        exchange,
    }
}

impl RowKey {
    fn units(&self, quantity: i64) -> i64 {
        from_vortex_quantity(quantity, &self.vortex_exchange, self.lot_size)
    }
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

/// One `/trading/orders` row (fields per the Vortex order book).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct VortexOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_number: String,
    #[serde(deserialize_with = "string_lenient")]
    pub ticker: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub error_reason: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub variety: String,
    #[serde(deserialize_with = "string_lenient")]
    pub validity: String,
    #[serde(deserialize_with = "string_lenient")]
    pub order_created_at: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub total_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub pending_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub traded_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub lot_size: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub order_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub trigger_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub traded_price: f64,
}

/// `map_order_data` + `transform_order_data`.
pub(crate) fn map_orders(rows: &[VortexOrder], symbols: &SymbolResolver) -> Vec<Order> {
    rows.iter()
        .map(|o| {
            let k = row_key(
                symbols,
                &o.ticker,
                &o.symbol,
                &o.exchange,
                &o.product,
                o.lot_size,
            );
            let status = map_status(&o.status);
            Order {
                order_id: o.order_id.clone(),
                exchange_order_id: non_empty(&o.order_number),
                side: o.transaction_type.trim().to_ascii_uppercase(),
                quantity: clamp_i32(k.units(o.total_quantity)),
                filled_quantity: clamp_i32(k.units(o.traded_quantity)),
                pending_quantity: clamp_i32(k.units(o.pending_quantity)),
                price: o.order_price,
                trigger_price: o.trigger_price,
                average_price: o.traded_price,
                order_type: price_type_from_variety(&o.variety),
                validity: if o.validity.is_empty() {
                    "DAY".into()
                } else {
                    o.validity.clone()
                },
                order_timestamp: o.order_created_at.clone(),
                exchange_timestamp: None,
                rejection_reason: if status == "rejected" {
                    non_empty(&o.error_reason)
                } else {
                    None
                },
                status,
                order_tag: None,
                symbol: k.symbol,
                exchange: k.exchange,
                product: k.product,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Trades
// ---------------------------------------------------------------------------

/// One `/trading/trades` row. Vortex may send only `order_id`, `trade_no`,
/// `trade_price`, `trade_quantity` and `traded_at`; such a row is completed
/// from its order (`fill_from`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct VortexTrade {
    #[serde(deserialize_with = "string_lenient")]
    pub order_id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trade_no: String,
    #[serde(deserialize_with = "string_lenient")]
    pub ticker: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "string_lenient")]
    pub traded_at: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub trade_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub lot_size: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub trade_price: f64,
}

impl VortexTrade {
    /// The web's `_TRADE_FIELDS_FROM_ORDER`: what a bare trade takes from
    /// its order. The trade's own fields are kept.
    pub fn fill_from(&mut self, o: &VortexOrder) {
        let fill = |mine: &mut String, theirs: &str| {
            if mine.is_empty() {
                *mine = theirs.to_string();
            }
        };
        fill(&mut self.ticker, &o.ticker);
        fill(&mut self.exchange, &o.exchange);
        fill(&mut self.transaction_type, &o.transaction_type);
        fill(&mut self.product, &o.product);
        if self.lot_size == 0 {
            self.lot_size = o.lot_size;
        }
    }
}

/// Rupee value of a fill: units x price, except an MCX contract quoted per
/// a smaller unit than it trades in (GOLDM: 100 g quoted per 10 g), valued
/// as lots x price x the quotation multiplier (web `trade_value`).
pub(crate) fn trade_value(
    units: i64,
    price: f64,
    exchange: &str,
    underlying: Option<&str>,
    lot_size: i64,
) -> f64 {
    if exchange != "MCX" || lot_size <= 0 {
        return units as f64 * price;
    }
    let multiplier = underlying
        .and_then(|u| {
            MCX_QUOTATION_MULTIPLIERS
                .iter()
                .find(|(root, _)| root.eq_ignore_ascii_case(u.trim()))
        })
        .map(|(_, m)| *m)
        .unwrap_or(lot_size);
    units as f64 / lot_size as f64 * price * multiplier as f64
}

/// `map_trade_data` + `transform_tradebook_data`.
pub(crate) fn map_trades(rows: &[VortexTrade], symbols: &SymbolResolver) -> Vec<Trade> {
    rows.iter()
        .map(|t| {
            let k = row_key(
                symbols,
                &t.ticker,
                &t.symbol,
                &t.exchange,
                &t.product,
                t.lot_size,
            );
            let units = k.units(t.trade_quantity);
            Trade {
                order_id: t.order_id.clone(),
                trade_id: t.trade_no.clone(),
                side: t.transaction_type.trim().to_ascii_uppercase(),
                quantity: clamp_i32(units),
                average_price: t.trade_price,
                trade_value: round2(trade_value(
                    units,
                    t.trade_price,
                    &k.exchange,
                    k.underlying.as_deref(),
                    k.lot_size,
                )),
                timestamp: t.traded_at.clone(),
                order_tag: None,
                symbol: k.symbol,
                exchange: k.exchange,
                product: k.product,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Positions
// ---------------------------------------------------------------------------

/// One row of `/trading/portfolio/positions` `data.net` (carried forward
/// plus today). Vortex sends no price; `ltp` is attached from a quotes call.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct VortexPosition {
    #[serde(deserialize_with = "string_lenient")]
    pub ticker: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub buy_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub sell_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub lot_size: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub multiplier: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub buy_value: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub sell_value: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ltp: f64,
}

impl VortexPosition {
    /// The open quantity in units: on MCX (and NSE_CUR) Vortex counts lots,
    /// and actual quantity = quantity x lot_size x multiplier.
    fn open_units(&self) -> f64 {
        let q = self.quantity as f64;
        if q != 0.0 && matches!(self.exchange.as_str(), "MCX_FO" | "NSE_CUR") {
            let or_one = |v: f64| if v == 0.0 { 1.0 } else { v };
            q * or_one(self.lot_size as f64) * or_one(self.multiplier)
        } else {
            q
        }
    }

    /// Whole P&L: sell value - buy value plus the open quantity at `ltp`.
    /// `None` for an open position with no price: unknown, not zero.
    pub fn pnl(&self) -> Option<f64> {
        let booked = self.sell_value - self.buy_value;
        let qty = self.open_units();
        if qty == 0.0 {
            Some(booked)
        } else if self.ltp == 0.0 {
            None
        } else {
            Some(booked + qty * self.ltp)
        }
    }

    /// `(realized, unrealized)`, summing to [`Self::pnl`]: the open quantity
    /// marked from its average price to `ltp` is unrealized, the rest is
    /// booked. `None` without a price or an average price.
    pub fn pnl_split(&self) -> Option<(f64, f64)> {
        let total = self.pnl()?;
        let qty = self.open_units();
        if qty == 0.0 {
            return Some((total, 0.0));
        }
        if self.average_price == 0.0 {
            return None;
        }
        let unrealized = qty * (self.ltp - self.average_price);
        Some((total - unrealized, unrealized))
    }
}

/// `map_position_data` + `transform_positions_data`.
pub(crate) fn map_positions(rows: &[VortexPosition], symbols: &SymbolResolver) -> Vec<Position> {
    rows.iter()
        .map(|p| {
            let k = row_key(
                symbols,
                &p.ticker,
                &p.symbol,
                &p.exchange,
                &p.product,
                p.lot_size,
            );
            let (realized, unrealized) = p.pnl_split().unwrap_or_default();
            Position {
                quantity: clamp_i32(k.units(p.quantity)),
                average_price_basis: None,
                overnight_quantity: 0,
                average_price: round2(p.average_price),
                ltp: round2(p.ltp),
                // An open row without a price has an unknown P&L; 0 is sent
                // because the contract wants a number.
                pnl: round2(p.pnl().unwrap_or(0.0)),
                realized_pnl: round2(realized),
                unrealized_pnl: round2(unrealized),
                buy_quantity: clamp_i32(k.units(p.buy_quantity)),
                buy_value: p.buy_value,
                sell_quantity: clamp_i32(k.units(p.sell_quantity)),
                sell_value: p.sell_value,
                symbol: k.symbol,
                exchange: k.exchange,
                product: k.product,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Holdings
// ---------------------------------------------------------------------------

/// The NSE or BSE listing of a holding.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Listing {
    #[serde(deserialize_with = "string_lenient")]
    pub ticker: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
}

/// One `/trading/portfolio/holdings` row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct VortexHolding {
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
    pub nse: Option<Listing>,
    pub bse: Option<Listing>,
    #[serde(deserialize_with = "f64_lenient")]
    pub total_free: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub t1_quantity: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub average_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_price: f64,
}

impl VortexHolding {
    /// The NSE listing, else the BSE one.
    pub fn listing(&self) -> Option<&Listing> {
        [&self.nse, &self.bse]
            .into_iter()
            .flatten()
            .find(|l| !l.ticker.trim().is_empty())
    }

    /// Free-to-sell (DP + pool) plus T1 unsettled.
    fn quantity(&self) -> i64 {
        (self.total_free + self.t1_quantity) as i64
    }
}

/// `map_portfolio_data` + `transform_holdings_data`. The OpenAlgo exchange
/// is the listing's, else its ticker prefix (`NSE:IDEA`).
pub(crate) fn map_holdings(rows: &[VortexHolding], symbols: &SymbolResolver) -> Vec<Holding> {
    rows.iter()
        .map(|h| {
            let (ticker, vortex_exchange) = h
                .listing()
                .map(|l| (l.ticker.as_str(), l.exchange.as_str()))
                .unwrap_or(("", ""));
            let exchange = if vortex_exchange.is_empty() {
                ticker.split_once(':').map(|(p, _)| p).unwrap_or("").into()
            } else {
                oa_exchange(vortex_exchange)
            };
            let brsymbol = split_ticker(ticker);
            let symbol = symbols
                .oa_symbol(brsymbol, &exchange)
                .or_else(|| non_empty(brsymbol))
                .unwrap_or_else(|| h.isin.clone());
            let qty = h.quantity();
            let (avg, ltp) = (h.average_price, h.last_price);
            let pnl = if ltp != 0.0 {
                (ltp - avg) * qty as f64
            } else {
                0.0
            };
            Holding {
                symbol,
                exchange,
                product: "CNC".into(),
                isin: non_empty(&h.isin),
                quantity: clamp_i32(qty),
                t1_quantity: clamp_i32(h.t1_quantity as i64),
                average_price: avg,
                ltp,
                close_price: 0.0,
                pnl: round2(pnl),
                pnl_percentage: if avg != 0.0 && ltp != 0.0 {
                    round2((ltp - avg) / avg * 100.0)
                } else {
                    0.0
                },
                current_value: round2(ltp * qty as f64),
            }
        })
        .collect()
}
