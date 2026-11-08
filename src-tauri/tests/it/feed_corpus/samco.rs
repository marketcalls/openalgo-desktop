//! Samco: the market feed, and order updates from polling the order book
//! (Samco has no order socket: web `_POLLING_BROKERS`), read from the
//! fake broker API by Samco's own `OrderPoller`.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::common::streaming::OrderFeed;
use openalgo_desktop_lib::brokers::samco::SamcoBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use serde_json::json;
use std::time::Duration;

/// Samco's order book, empty until a case serves another.
const ORDER_BOOK: &str = "/order/orderBook";

fn feeds(env: &Env) -> Feeds {
    let broker = SamcoBroker::with_urls(
        env.symbols(),
        env.http_url(),
        format!("{}/master", env.http_url()),
        env.url("main"),
    );
    let auth = AuthToken::new("session");
    let feeds = Feeds::boxed(broker.create_feed(&auth).expect("Samco's market feed"));
    if !env.orders() {
        return feeds;
    }
    env.serve_json(
        ORDER_BOOK,
        json!({"status": "Success", "orderBookDetails": []}),
    );
    let updates = broker
        .start_order_updates(&auth, Duration::from_secs(1))
        .expect("Samco's order poller");
    feeds
        .with_order_feed(OrderFeed::Stream(updates))
        .keeping(broker)
}

corpus_cases!("samco", feeds, [order_book_polling]);
