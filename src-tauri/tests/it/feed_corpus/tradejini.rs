//! Tradejini: the NxtradStream binary feed
//! (`brokers::tradejini::streaming::TradejiniFeed`), L1 quotes and L5
//! books on one socket. The web streams no Tradejini order updates
//! (`services/order_update_service.py` has no Tradejini adapter).
//!
//! The feed is the one `TradejiniBroker::create_feed` builds for a session
//! (the API key and access token in the URL), pointed at the fake socket,
//! and the broker wires the bridge's depth levels and refusals as a
//! session does.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::tradejini::TradejiniBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    // The stream's own path (`streaming::STREAM_URL`) on the fake socket.
    let broker = Arc::new(TradejiniBroker::with_urls(
        env.symbols(),
        env.http_url(),
        format!("{}/v2.1/stream", env.url("main")),
    ));
    let feed = broker
        .create_feed(&AuthToken::new("key:tok"))
        .expect("Tradejini's market data feed");
    Feeds::boxed(feed).with_broker(broker)
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
    ]
);
