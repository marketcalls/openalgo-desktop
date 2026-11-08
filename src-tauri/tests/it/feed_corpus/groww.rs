//! Groww: the NATS-over-WebSocket market feed as `GrowwBroker::create_feed`
//! builds it. Its `prepare` mints a socket token over REST before every
//! connect, read here from the fake broker API.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::groww::GrowwBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use serde_json::json;

/// Groww's socket-token endpoint (`streaming::SOCKET_TOKEN_URL`'s path).
const SOCKET_TOKEN: &str = "/v1/api/apex/v1/socket/token/create/";

fn feeds(env: &Env) -> Feeds {
    env.serve_json(
        SOCKET_TOKEN,
        json!({"token": "socket-jwt", "subscriptionId": "sub-1"}),
    );
    let broker = GrowwBroker::with_base_url(env.symbols(), env.http_url()).with_feed_endpoints(
        &format!("{}{}", env.http_url(), SOCKET_TOKEN),
        &env.url("main"),
    );
    let auth = AuthToken::new("login-token");
    Feeds::boxed(broker.create_feed(&auth).expect("Groww's market feed"))
}

corpus_cases!(
    "groww",
    feeds,
    [
        quote_merges_sparse_ticks,
        ltp_mode,
        depth_book_price_and_clear,
        index_quote_and_ltp,
        index_depth_falls_back_to_price,
        derivative_quote_without_oi,
        reconnect_starts_afresh,
    ]
);
