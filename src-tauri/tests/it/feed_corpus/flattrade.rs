//! Flattrade: the Noren market data socket as a Flattrade session makes it
//! (`noren_family::feeds`), carrying the `om` order updates too after
//! `{"t":"o"}` (PiConnect allows one session per user).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::flattrade;

fn feeds(env: &Env) -> Feeds {
    super::noren_family::feeds(env, flattrade::config())
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
        new_price_without_times,
        quote_without_order_flow,
    ]
);
