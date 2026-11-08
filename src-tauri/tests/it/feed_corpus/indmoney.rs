//! INDmoney (INDstocks): the price socket (`IndmoneyFeed`) and the
//! order-update socket (`IndmoneyOrderFeed`), which maps the stream's bare
//! order numbers to canonical `EQ-` ids through the adapter's
//! `OrderIdMap`.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::indmoney::streaming::{IndmoneyFeed, IndmoneyOrderFeed};
use openalgo_desktop_lib::brokers::indmoney::OrderIdMap;
use parking_lot::Mutex;
use std::sync::Arc;

/// The orders the desktop placed (or read in an order book) before the
/// case's updates arrive, as `orders::place_order` and `raw_order_book`
/// remember them. `96057999` (case `order_unknown_id`) is not among them:
/// it was placed outside the desktop (audit RB-11).
const KNOWN_ORDERS: &[&str] = &["EQ-96057848", "EQ-96057849", "EQ-96057850"];

fn feeds(env: &Env) -> Feeds {
    let mut ids = OrderIdMap::default();
    for id in KNOWN_ORDERS {
        ids.remember(id);
    }
    Feeds::market(IndmoneyFeed::new(&env.url("main"), "t")).with_orders(IndmoneyOrderFeed::new(
        &env.url("orders"),
        "t",
        Arc::new(Mutex::new(ids)),
    ))
}

corpus_cases!(
    "indmoney",
    feeds,
    [
        ltp_mode,
        quote_mode,
        index_quote,
        future_quote_oi,
        depth_from_best_bid_ask,
        reconnect_starts_afresh,
        ltp_quote_mode_change,
        order_lifecycle,
        order_unknown_id,
    ]
);
