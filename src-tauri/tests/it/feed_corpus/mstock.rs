//! mStock: the market data socket (`brokers::mstock::streaming::MstockFeed`),
//! with the session's `MstockBroker` against the fakes so the bridge takes
//! its depth levels and refusals as a session does. mStock has no
//! order-update stream, on the web or here.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::mstock::streaming::MstockFeed;
use openalgo_desktop_lib::brokers::mstock::MstockBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = MstockBroker::with_urls(
        env.symbols(),
        env.http_url(),
        env.http_url(),
        env.url("main"),
    );
    Feeds::market(MstockFeed::new(&env.url("main"), "jwt", "key"))
        .with_broker(Arc::new(broker), &AuthToken::new("jwt"))
}

corpus_cases!(
    "mstock",
    feeds,
    [
        ltp_mode,
        quote_mode,
        depth_full_packets,
        full_trade_time_in_seconds,
        same_token_two_segments,
        index_quote,
        future_quote_to_depth,
        currency_segment,
        depth_levels_refused,
        reconnect_replays,
    ]
);
