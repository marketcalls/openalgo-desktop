//! Sahi: the protobuf market data stream (`brokers::sahi::streaming::SahiFeed`),
//! built by the broker as a session builds it, so the broker's feed
//! refusal answers subscribes on an exchange Sahi does not stream. Sahi
//! streams no order updates, on the web or here.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::sahi::SahiBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(SahiBroker::with_urls(
        env.symbols(),
        env.http_url(),
        env.http_url(),
        format!("{}/master", env.http_url()),
        env.url("main"),
    ));
    let feed = broker
        .create_feed(&AuthToken::new("t"))
        .expect("Sahi's market feed");
    Feeds::boxed(feed).with_broker(broker)
}

corpus_cases!(
    "sahi",
    feeds,
    [
        ltp_mode,
        quote_mode,
        depth_mode,
        index_quote,
        derivative_oi,
        reconnect_resubscribes,
        unsupported_exchange,
    ]
);
