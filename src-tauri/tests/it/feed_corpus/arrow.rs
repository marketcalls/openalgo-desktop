//! Arrow: the market data socket (`ArrowFeed`) and the order-update socket
//! (`ArrowOrderFeed`), both authenticated by the query (`appID`, `token`),
//! as `ArrowBroker::create_feed` and `create_order_feed` build them for a
//! session (`appID:token`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::arrow::{ArrowBroker, ArrowUrls};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(ArrowBroker::with_base_url(
        env.symbols(),
        ArrowUrls {
            rest: env.http_url(),
            history: env.http_url(),
            ws_market: env.url("main"),
            ws_orders: env.url("orders"),
        },
    ));
    let auth = AuthToken::new("app:jwt");
    let feed = broker.create_feed(&auth).expect("Arrow's market feed");
    let orders = broker.create_order_feed(&auth).expect("Arrow's order feed");
    Feeds::boxed(feed)
        .with_order_feed(orders)
        .with_broker(broker, &auth)
}

corpus_cases!(
    "arrow",
    feeds,
    [
        ltp_mode,
        quote_mode,
        full_depth,
        index_ltp_quote,
        future_quote_then_depth,
        reconnect_replays,
        order_lifecycle,
        order_stop_loss_margin,
        order_symbol_by_token,
    ]
);
