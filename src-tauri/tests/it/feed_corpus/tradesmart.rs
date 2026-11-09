//! TradeSmart: the Noren market data socket as a TradeSmart session makes
//! it (`noren_family::feeds`). TradeSmart pushes the `om` order updates on
//! it once the login is accepted, without `{"t":"o"}`.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::tradesmart;

fn feeds(env: &Env) -> Feeds {
    super::noren_family::feeds(env, tradesmart::config())
}

corpus_cases!(
    "tradesmart",
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
