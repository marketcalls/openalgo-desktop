//! Pocketful: the market data socket (`PocketfulFeed`), which also carries
//! order and trade updates (packet modes 50 and 51).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::pocketful::streaming::PocketfulFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(PocketfulFeed::new(
        &env.url("main"),
        "c",
        "t",
        env.symbols(),
    ))
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
        order_packets,
    ]
);
