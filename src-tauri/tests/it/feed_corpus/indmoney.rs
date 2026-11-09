//! INDmoney (INDstocks): the price socket, from the broker as a session
//! builds it (`IndmoneyBroker::create_feed`, with its exchange refusals),
//! and the order-update socket (`IndmoneyOrderFeed`), which maps the
//! stream's bare order numbers to canonical `EQ-` ids through the
//! adapter's `OrderIdMap`.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::indmoney::streaming::IndmoneyOrderFeed;
use openalgo_desktop_lib::brokers::indmoney::{Endpoints, IndmoneyBroker, OrderIdMap};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use parking_lot::Mutex;
use std::sync::Arc;

/// The orders the desktop placed (or read in an order book) before the
/// case's updates arrive, as `orders::place_order` and `raw_order_book`
/// remember them. `96057999` (case `order_unknown_id`) is not among them:
/// it was placed outside the desktop (audit RB-11).
const KNOWN_ORDERS: &[&str] = &["EQ-96057848", "EQ-96057849", "EQ-96057850"];

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(IndmoneyBroker::with_endpoints(
        env.symbols(),
        Endpoints {
            api: env.http_url(),
            prices_ws: env.url("main"),
            orders_ws: env.url("orders"),
        },
    ));
    let auth = AuthToken::new("t");
    let market = broker.create_feed(&auth).expect("INDmoney's price feed");
    let mut ids = OrderIdMap::default();
    for id in KNOWN_ORDERS {
        ids.remember(id);
    }
    Feeds::boxed(market)
        .with_orders(IndmoneyOrderFeed::new(
            &env.url("orders"),
            "t",
            Arc::new(Mutex::new(ids)),
        ))
        .with_broker(broker, &auth)
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
        exchange_refused,
        reconnect_starts_afresh,
        ltp_quote_mode_change,
        order_lifecycle,
        order_unknown_id,
    ]
);
