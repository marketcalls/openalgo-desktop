//! Zebu: the Noren market data socket (`NorenFeed` on Zebu's config),
//! which carries the `om` order updates too after `{"t":"o"}`.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::families::noren::streaming::NorenFeed;
use openalgo_desktop_lib::brokers::zebu;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(NorenFeed::new(
        zebu::config(),
        &env.url("main"),
        "U1",
        "tok",
        env.symbols(),
    ))
}

corpus_cases!(
    "zebu",
    feeds,
    [
        quote_snapshot_and_update,
        index_ltp_and_quote,
        depth_future_oi,
        reconnect_replays,
        order_updates,
        frame_without_exchange,
    ]
);
