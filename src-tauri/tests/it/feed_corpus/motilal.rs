//! Motilal Oswal: the binary broadcast feed and the JSON order-update
//! socket, as `MotilalBroker::create_feed` and `create_order_feed` build
//! them from a session (`AuthToken:::accesstoken:::clientcode:::apikey`),
//! with the broker installed as a session installs it.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::motilal::MotilalBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(MotilalBroker::with_urls(
        env.symbols(),
        env.http_url(),
        env.url("main"),
        env.url("orders"),
    ));
    let auth = AuthToken::new("at:::acc:::C1:::k");
    Feeds::boxed(broker.create_feed(&auth).expect("Motilal's market feed"))
        .with_order_feed(
            broker
                .create_order_feed(&auth)
                .expect("Motilal's order socket"),
        )
        .with_broker(broker, &auth)
}

corpus_cases!(
    "motilal",
    feeds,
    [
        ltp_mode,
        quote_trade_before_ohlc,
        quote_ohlc_before_first_trade,
        depth_before_the_book,
        depth_book_before_the_quote,
        derivative_depth_oi,
        future_quote_oi,
        index_quote,
        reconnect_quote,
        order_lifecycle,
        order_session_refused,
        depth_20_refused,
        market_logout,
        mode_change,
        wire_key_collision,
        release_during_outage,
    ]
);
