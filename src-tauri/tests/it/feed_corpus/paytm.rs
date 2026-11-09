//! Paytm Money: the market data socket (`PaytmFeed`), authenticated by the
//! query (`x_jwt_token`). The web streams no Paytm order updates.
//!
//! The feed is the one a session gets (`PaytmBroker::create_feed`, on the
//! session's public access token), and the broker's Depth refusal answers
//! a Depth 20 subscribe as the web does.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::paytm::PaytmBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(PaytmBroker::with_base_url(
        env.symbols(),
        env.http_url(),
        env.http_url(),
        env.url("main"),
    ));
    let auth = AuthToken::new("rest").with_feed(Some("pat"));
    let feed = broker.create_feed(&auth).expect("Paytm's market feed");
    Feeds::boxed(feed).with_broker(broker, &auth)
}

corpus_cases!(
    "paytm",
    feeds,
    [
        ltp_mode,
        quote_mode,
        full_depth,
        index_ltp,
        index_quote,
        index_full,
        future_quote_then_depth,
        reconnect_replays,
        depth_20_refused_by_the_web,
    ]
);
