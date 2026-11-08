//! IBulls: the XTS market data feed with its own configuration
//! (`brokers::ibulls::CONFIG`; the family's feed in [`super::xts`]).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::ibulls::CONFIG;

fn feeds(env: &Env) -> Feeds {
    super::xts::feeds(&CONFIG, env)
}

corpus_cases!(
    "ibulls",
    feeds,
    [
        depth_five_and_clear,
        depth_twenty_streams_five,
        future_depth_oi,
        index_quote,
        ltp_equity_and_index,
        quote_partial_zeros,
        quote_touchline,
        reconnect_replays,
        text_1105,
        unsubscribe_keeps_the_others,
    ]
);
