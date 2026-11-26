//! Sahi: the protobuf market data stream (`brokers::sahi::streaming::SahiFeed`),
//! built by the broker as a session builds it, so the broker's feed
//! refusal answers subscribes on an exchange Sahi does not stream, and the
//! order stream (`SahiOrderFeed`, web openalgo#2474) from
//! `Broker::create_order_feed`, pointed at the `orders` socket.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::sahi::SahiBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let (symbols, http) = (env.symbols(), env.http_url());
    let at = move |url: &str| {
        SahiBroker::with_urls(
            symbols.clone(),
            http.clone(),
            http.clone(),
            format!("{}/master", http),
            url.to_string(),
        )
    };
    let broker = Arc::new(at(&env.url("main")));
    let auth = AuthToken::new("t");
    let feed = broker.create_feed(&auth).expect("Sahi's market feed");
    let pool_auth = auth.clone();
    let pool = at.clone();
    let feeds = Feeds::boxed(feed)
        .with_pool(move |url| {
            pool(url)
                .create_feed(&pool_auth)
                .expect("Sahi's market feed for another connection")
        })
        .with_broker(broker, &auth);
    if !env.orders() {
        return feeds;
    }
    let orders = at(&env.url("main"))
        .with_order_stream_url(env.url("orders"))
        .create_order_feed(&auth)
        .expect("Sahi's order stream");
    feeds.with_order_feed(orders)
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
        order_lifecycle,
        order_stream_reconnects,
    ]
);
