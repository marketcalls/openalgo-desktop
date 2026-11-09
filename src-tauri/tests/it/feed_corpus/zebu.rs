//! Zebu: the Noren market data socket as a Zebu session makes it
//! (`noren_family::feeds`), carrying the `om` order updates too after
//! `{"t":"o"}`.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::zebu;

fn feeds(env: &Env) -> Feeds {
    super::noren_family::feeds(env, zebu::config())
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
        new_price_without_times,
        reconnect_starts_afresh,
        quote_without_order_flow,
    ]
);
