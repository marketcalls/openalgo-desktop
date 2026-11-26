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
//! host; here it is built for the fake order socket. An order event that
//! names only Tradejini's trading symbol takes the symbol of its order book
//! row, read from the fake broker API (`Broker::complete_order_update`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::tradejini::order_feed::TradejiniOrderFeed;
use openalgo_desktop_lib::brokers::tradejini::TradejiniBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use serde_json::json;
use std::sync::Arc;

/// The stream's own path (`streaming::STREAM_URL`) on a fake socket.
fn stream(env: &Env, socket: &str) -> String {
    format!("{}/v2.1/stream", env.url(socket))
}

/// The account's order book as Tradejini's REST API answers it
/// (`symDetails=true`): the order of `order_lifecycle` whose event names
/// only the trading symbol (`25112500000104`), with its scrip id.
fn order_book() -> serde_json::Value {
    json!({"s": "ok", "d": [{
        "sym": {"id": "EQT_SBIN_EQ_NSE", "exchange": "NSE", "symbol": "SBIN", "tradSymbol": "SBIN-EQ"},
        "symId": "EQT_SBIN_EQ_NSE",
        "orderId": "25112500000104", "orderTime": "25-11-2026 09:16:00",
        "qty": 5, "side": "sell", "type": "stopmarket", "product": "delivery",
        "status": "trigger pending", "limitPrice": 0, "trigPrice": 780, "avgPrice": 0,
        "fillQty": 0, "pendingQty": 5, "validity": "day", "reason": ""
    }]})
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
    if env.orders() {
        env.serve_json("/api/oms/orders", order_book());
    }
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
