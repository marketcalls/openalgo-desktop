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
    let (symbols, http) = (env.symbols(), env.http_url());
    let at = move |url: &str| {
        Arc::new(SahiBroker::with_urls(
            symbols.clone(),
            http.clone(),
            http.clone(),
            format!("{}/master", http),
            url.to_string(),
        ))
    };
    let broker = at(&env.url("main"));
    let auth = AuthToken::new("t");
    let feed = broker.create_feed(&auth).expect("Sahi's market feed");
    let pool_auth = auth.clone();
    Feeds::boxed(feed)
        .with_pool(move |url| {
            at(url)
                .create_feed(&pool_auth)
                .expect("Sahi's market feed for another connection")
        })
        .with_broker(broker, &auth)
}

corpus_cases!(
    "sahi",
    feeds,
    [
        ltp_mode,
        quote_mode,
        depth_mode,
        index_quote,
        index_depth,
        derivative_oi,
        reconnect_resubscribes,
        pool_connections_keep_their_own_state,
        close_token_expired_stops,
        close_too_many_connections_waits,
        close_rate_limited_upgrade_waits,
        close_ping_timeout_reconnects,
        unsupported_exchange,
    ]
);
