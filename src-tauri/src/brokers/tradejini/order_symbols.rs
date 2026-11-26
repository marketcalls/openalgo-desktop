//! The OpenAlgo symbol of an order update that names Tradejini's trading
//! symbol only (review of the port of marketcalls/openalgo#2477: a web
//! defect not copied).
//!
//! Tradejini documents the `orders` event with `trdSym` (`SBIN-EQ`) and no
//! `symId`, while the symbol master keys Tradejini's instruments by the
//! scrip id (`EQT_SBIN_EQ_NSE`, `master_contract`). The stream's own lookup
//! (`order_feed`) then finds nothing, and the web sends the update with the
//! broker's symbol, where the order stream's `symbol` is the OpenAlgo one
//! (`docs/api/websocket-streaming/order-updates.md`): a client that matches
//! updates by symbol (a Flow order watch, an SDK callback) never sees them.
//! The order book row of the same order carries the scrip id, so the relay
//! gives the update the symbol the order book gives that order
//! (`Broker::complete_order_update`, as INDmoney completes its order ids).
//!
//! One book read names every order it lists, and they are kept (bounded,
//! expiring, cleared at logout; `docs/port/no-keychain.md`, caches), so the
//! later updates of those orders read nothing. An order the book does not
//! name, or a read that fails or is cut short, is not read again for that
//! order within `BOOK_READ_EVERY`; its updates keep the broker's symbol
//! meanwhile, as the web sends them.

use super::{orders, TradejiniBroker};
use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{AuthToken, Order};
use crate::cache::TtlCache;
use std::time::{Duration, Instant};

/// Orders kept at once: more than a trading day's orders.
const ORDERS_KEPT: usize = 4096;

/// How long an order's symbol is kept: past the end of its trading day.
const KEPT_FOR: Duration = Duration::from_secs(12 * 60 * 60);

/// The shortest time between two book reads for one order, so a burst of
/// updates for an order the book does not list yet reads it once.
const BOOK_READ_EVERY: Duration = Duration::from_secs(5);

/// Order id -> the OpenAlgo symbol its order book row gives; `None` while a
/// recent read for that order named nothing.
pub(crate) struct OrderSymbols(TtlCache<String, Option<String>>);

impl Default for OrderSymbols {
    fn default() -> Self {
        Self(TtlCache::new(ORDERS_KEPT, KEPT_FOR))
    }
}

impl OrderSymbols {
    pub(crate) fn clear(&self) {
        self.0.clear();
    }
}

/// Whether `symbol` is a symbol of the master on `exchange`, which is how
/// an update the stream resolved shows.
fn is_openalgo(symbols: &SymbolResolver, symbol: &str, exchange: &str) -> bool {
    !symbol.is_empty() && symbols.by_symbol(exchange, symbol).is_some()
}

/// `u` with the OpenAlgo symbol of its order, when the stream could not
/// name it and the order book can.
pub(super) async fn complete(
    b: &TradejiniBroker,
    auth: &AuthToken,
    mut u: OrderUpdate,
) -> OrderUpdate {
    if u.orderid.is_empty() || is_openalgo(b.resolver(), &u.symbol, &u.exchange) {
        return u;
    }
    let (kept, generation) = b
        .order_symbols
        .0
        .get_with_generation(&u.orderid, Instant::now());
    let symbol = match kept {
        Some(kept) => kept,
        None => read_book(b, auth, &u.orderid, generation).await,
    };
    if let Some(symbol) = symbol {
        u.symbol = symbol;
    }
    u
}

/// The order book's symbol for `orderid`, learning every order it names.
async fn read_book(
    b: &TradejiniBroker,
    auth: &AuthToken,
    orderid: &str,
    generation: u64,
) -> Option<String> {
    let kept = &b.order_symbols.0;
    // Claimed before the read: a read that fails, or that the relay stops
    // waiting for, is not tried again for this order at once.
    kept.fill_for(
        orderid.to_string(),
        None,
        generation,
        Instant::now(),
        BOOK_READ_EVERY,
    );
    match orders::get_order_book(b, auth).await {
        Ok(book) => learn(kept, b.resolver(), &book, orderid, generation),
        Err(e) => {
            tracing::warn!(
                "Tradejini order {} keeps the broker's symbol: the order book could not be read ({})",
                orderid,
                e.code()
            );
            None
        }
    }
}

/// Keep the symbol of every order in `book` that the master names, and
/// return `orderid`'s. A logout since `generation` keeps nothing
/// (`TtlCache::fill`); the answer still goes to the update that asked.
fn learn(
    kept: &TtlCache<String, Option<String>>,
    symbols: &SymbolResolver,
    book: &[Order],
    orderid: &str,
    generation: u64,
) -> Option<String> {
    let now = Instant::now();
    let mut found = None;
    for o in book
        .iter()
        .filter(|o| !o.order_id.is_empty() && is_openalgo(symbols, &o.symbol, &o.exchange))
    {
        kept.fill(o.order_id.clone(), Some(o.symbol.clone()), generation, now);
        if o.order_id == orderid {
            found = Some(o.symbol.clone());
        }
    }
    found
}
