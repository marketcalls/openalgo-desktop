//! Wisdom Capital: the XTS market data feed with its own configuration
//! (`brokers::wisdom::CONFIG`; the family's feed in [`super::xts`]).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::wisdom::CONFIG;

fn feeds(env: &Env) -> Feeds {
    super::xts::feeds(&CONFIG, env)
}

corpus_cases!(
    "wisdom",
    feeds,
    [
        depth_five_and_clear,
        depth_twenty_streams_five,
        future_depth_oi,
        index_quote,
        ltp_equity_and_index,
        quote_partial_zeros,
        quote_touchline_and_root,
        reconnect_replays,
        text_1105,
        unsubscribe_keeps_the_others,
    ]
);
