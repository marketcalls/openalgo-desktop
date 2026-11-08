//! INDmoney (INDstocks): the price socket and the order-update socket,
//! both from the broker as a session builds them (`create_feed`, with its
//! exchange refusals, and `order_socket`). The order socket names an order
//! by its bare number; the broker maps it to the canonical `EQ-` id from
//! what it placed or read, else from the order book on the fake broker API
//! (`Broker::complete_order_update`, audit RB-11).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::common::streaming::OrderFeed;
use openalgo_desktop_lib::brokers::indmoney::{Endpoints, IndmoneyBroker};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use serde_json::json;
use std::sync::Arc;

/// The account's order book as INDmoney's REST API answers it: the
/// orders of the cases, `96057999` (case `order_unknown_id`) among them,
/// placed outside the desktop.
fn order_book() -> serde_json::Value {
    let ids = ["EQ-96057848", "EQ-96057849", "EQ-96057850", "EQ-96057999"];
    json!({"status": "success", "data": ids.map(|id| json!({"id": id}))})
}

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
    let feeds = Feeds::boxed(market).with_broker(broker.clone(), &auth);
    if !env.orders() {
        return feeds;
    }
    env.serve_json("/order-book", order_book());
    let orders = broker
        .order_socket(&auth)
        .expect("INDmoney's order-update socket");
    feeds.with_order_feed(OrderFeed::Socket(orders))
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
