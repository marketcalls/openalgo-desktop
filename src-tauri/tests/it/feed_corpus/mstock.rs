//! mStock: the market data socket (`brokers::mstock::streaming::MstockFeed`).
//! mStock has no order-update stream, on the web or here.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::mstock::streaming::MstockFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(MstockFeed::new(&env.url("main"), "jwt", "key"))
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
        reconnect_replays,
    ]
);
