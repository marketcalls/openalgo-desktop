//! Rupeezy: the Vortex price socket (`RupeezyFeed`), authenticated by the
//! query (`auth_token`). The web streams no Rupeezy order updates.
//!
//! The feed is the one a session gets (`RupeezyBroker::create_feed`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::rupeezy::{RupeezyBroker, RupeezyUrls};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(RupeezyBroker::with_base_url(
        env.symbols(),
        RupeezyUrls {
            rest: env.http_url(),
            master: env.http_url(),
            ws: env.url("main"),
        },
    ));
    let auth = AuthToken::new("tok");
    let feed = broker.create_feed(&auth).expect("Rupeezy's market feed");
    Feeds::boxed(feed).with_broker(broker, &auth)
}

corpus_cases!(
    "rupeezy",
    feeds,
    [
        ltp_mode,
        quote_mode,
        full_depth,
        index_quote,
        future_ltp_and_depth,
        reconnect_replays,
        quote_cut_and_unknown_packets,
    ]
);
