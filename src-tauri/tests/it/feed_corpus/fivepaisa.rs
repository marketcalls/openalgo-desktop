//! 5paisa (the native feed, not 5paisa XTS): the market feed
//! `FivepaisaBroker::create_feed` makes, and order updates from polling the
//! order book (5paisa evicts a second feed connection, so the web polls:
//! `_POLLING_BROKERS`), read from the fake broker API by 5paisa's own
//! `OrderPoller`.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::common::streaming::OrderFeed;
use openalgo_desktop_lib::brokers::fivepaisa::orders::ORDER_BOOK;
use openalgo_desktop_lib::brokers::fivepaisa::FivepaisaBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn feeds(env: &Env) -> Feeds {
    // The broker wires the bridge as a session does (its depth levels and
    // exchange refusals) and owns the order poller.
    let broker = Arc::new(
        FivepaisaBroker::with_urls(
            env.symbols(),
            env.http_url(),
            format!("{}/master", env.http_url()),
        )
        // The production path (`feeds/api/chat`) on the fake broker's socket.
        .with_feed_url(format!("{}/feeds/api/chat", env.url("main"))),
    );
    // api key ::: client code ::: access token, fake values.
    let auth = AuthToken::new("key:::c:::t");
    let feeds = Feeds::boxed(broker.create_feed(&auth).expect("5paisa's market feed"))
        .with_broker(broker.clone(), &auth);
    if !env.orders() {
        return feeds;
    }
    // The order book is empty until the case serves another.
    env.serve_json(
        ORDER_BOOK,
        json!({
            "head": {"statusDescription": "Success"},
            "body": {"Message": "Success", "OrderBookDetail": [], "Status": 0}
        }),
    );
    let updates = broker
        .start_order_updates(&auth, Duration::from_secs(1))
        .expect("5paisa's order poller");
    feeds.with_order_feed(OrderFeed::Stream(updates))
}

corpus_cases!(
    "fivepaisa",
    feeds,
    [
        ltp_mode,
        quote_mode,
        depth_after_quote,
        depth_before_quote,
        same_token_two_exchanges,
        future_mode_change,
        index_quote,
        reconnect_after_held_prices,
        order_book_polling,
        depth_20_refused,
    ]
);
