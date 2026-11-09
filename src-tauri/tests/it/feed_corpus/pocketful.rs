//! Pocketful: the market data socket (`PocketfulFeed`), which also carries
//! order and trade updates (packet modes 50 and 51).
//!
//! The feed is the one a session gets (`PocketfulBroker::create_feed`, on
//! the session's token and the client id stored with it).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::pocketful::PocketfulBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(PocketfulBroker::with_base_url(
        env.symbols(),
        env.http_url(),
        env.url("main"),
        env.http_url(),
    ));
    let auth = AuthToken::new("t").with_user_id("c");
    let feed = broker.create_feed(&auth).expect("Pocketful's market feed");
    Feeds::boxed(feed).with_broker(broker, &auth)
}

corpus_cases!(
    "pocketful",
    feeds,
    [
        ltp_compact,
        quote_detailed,
        depth_snapquote,
        depth_20_refused_on_the_web,
        index_quote,
        future_quote_oi,
        reconnect_replays,
        two_clients_ltp_and_quote,
        snapquote_average_beside_a_trade_price,
        quote_untimed_packet,
        order_packets,
    ]
);
