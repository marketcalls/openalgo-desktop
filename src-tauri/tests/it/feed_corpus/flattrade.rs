//! Flattrade: the Noren market data socket (`NorenFeed` on Flattrade's
//! PiConnect config), which carries the `om` order updates too after
//! `{"t":"o"}` (PiConnect allows one session per user).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::families::noren::streaming::NorenFeed;
use openalgo_desktop_lib::brokers::flattrade;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(NorenFeed::new(
        flattrade::config(),
        &env.url("main"),
        "U1",
        "tok",
        env.symbols(),
    ))
}

corpus_cases!(
    "flattrade",
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
