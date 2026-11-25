//! Tradejini: the NxtradStream binary feed
//! (`brokers::tradejini::streaming::TradejiniFeed`), L1 quotes and L5
//! books on one socket, and the order updates
//! (`brokers::tradejini::order_feed::TradejiniOrderFeed`): the stream's
//! `orders` events on a socket of their own (web openalgo#2477).
//!
//! The market feed is the one `TradejiniBroker::create_feed` builds for a
//! session (the API key and access token in the URL), pointed at the fake
//! socket, and the broker wires the bridge's depth levels and refusals as a
//! session does. The order socket is the same stream path with the same
//! session token, which `create_order_feed` points at the market feed's
//! host; here it is built for the fake order socket.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::tradejini::order_feed::TradejiniOrderFeed;
use openalgo_desktop_lib::brokers::tradejini::TradejiniBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

/// The stream's own path (`streaming::STREAM_URL`) on a fake socket.
fn stream(env: &Env, socket: &str) -> String {
    format!("{}/v2.1/stream", env.url(socket))
}

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(TradejiniBroker::with_urls(
        env.symbols(),
        env.http_url(),
        stream(env, "main"),
    ));
    let auth = AuthToken::new("key:tok");
    let feed = broker
        .create_feed(&auth)
        .expect("Tradejini's market data feed");
    let orders = TradejiniOrderFeed::new(&stream(env, "orders"), "key", "tok", env.symbols());
    Feeds::boxed(feed)
        .with_broker(broker, &auth)
        .with_orders(orders)
}

corpus_cases!(
    "tradejini",
    feeds,
    [
        quote_mode,
        ltp_mode,
        depth_waits_for_the_quote,
        depth_book_update_and_clear,
        depth_to_quote_and_back,
        reconnect_drops_the_book,
        index_quote_and_ltp,
        future_quote_oi,
        future_depth_oi,
        currency_quote_divisor,
        depth_20_30_50_refused,
        order_lifecycle,
        order_reconnect,
        order_session_refused,
    ]
);
