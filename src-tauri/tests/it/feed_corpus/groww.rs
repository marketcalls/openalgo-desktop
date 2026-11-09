//! Groww: the NATS-over-WebSocket market feed as `GrowwBroker::create_feed`
//! builds it, and order updates from polling the order book (Groww has no
//! order socket: web `_POLLING_BROKERS`), read by Groww's own
//! `OrderPoller`. The feed's `prepare` mints a socket token over REST
//! before every connect; the token and the order lists come from the fake
//! broker API (the CASH and FNO lists from the case's `http` answers). The
//! broker goes in too, so the exchanges it refuses are refused as the app
//! refuses them (`Broker::feed_refusal`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::common::streaming::OrderFeed;
use openalgo_desktop_lib::brokers::groww::GrowwBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Groww's socket-token endpoint (`streaming::SOCKET_TOKEN_URL`'s path).
const SOCKET_TOKEN: &str = "/v1/api/apex/v1/socket/token/create/";

fn feeds(env: &Env) -> Feeds {
    env.serve_json(
        SOCKET_TOKEN,
        json!({"token": "socket-jwt", "subscriptionId": "sub-1"}),
    );
    let broker = Arc::new(
        GrowwBroker::with_base_url(env.symbols(), env.http_url()).with_feed_endpoints(
            &format!("{}{}", env.http_url(), SOCKET_TOKEN),
            &env.url("main"),
        ),
    );
    let auth = AuthToken::new("login-token");
    let feeds = Feeds::boxed(broker.create_feed(&auth).expect("Groww's market feed"))
        .with_broker(broker.clone(), &auth);
    if !env.orders() {
        return feeds;
    }
    let updates = broker
        .start_order_updates(&auth, Duration::from_secs(1))
        .expect("Groww's order poller");
    feeds.with_order_feed(OrderFeed::Stream(updates))
}

corpus_cases!(
    "groww",
    feeds,
    [
        quote_merges_sparse_ticks,
        ltp_mode,
        depth_book_price_and_clear,
        quote_and_depth_book_first,
        index_quote_and_ltp,
        index_depth_falls_back_to_price,
        derivative_quote_without_oi,
        reconnect_starts_afresh,
        unsupported_exchanges,
        deliberate_disconnect_unsubscribes,
        order_book_polling,
        order_book_read_fails,
    ]
);
