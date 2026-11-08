//! Zerodha: the Kite ticker (`brokers::zerodha::streaming::KiteFeed`),
//! which carries order postbacks on the market data socket too.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::zerodha::streaming::KiteFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(KiteFeed::with_url(
        &env.url("main"),
        "k",
        "t",
        env.symbols(),
    ))
}

corpus_cases!(
    "zerodha",
    feeds,
    [
        quote_mode,
        ltp_mode,
        full_depth_future,
        index_quote,
        index_full,
        two_clients_mode_change,
        reconnect_replays,
        order_postbacks,
    ]
);
