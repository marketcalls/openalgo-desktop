//! RMoney: the XTS market data feed with its own configuration
//! (`brokers::rmoney::CONFIG`; the family's feed in [`super::xts`]).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::rmoney::CONFIG;

fn feeds(env: &Env) -> Feeds {
    super::xts::feeds(&CONFIG, env)
}

corpus_cases!(
    "rmoney",
    feeds,
    [
        depth_five_and_clear,
        depth_twenty_streams_five,
        event_name_supplies_the_code,
        future_depth_oi,
        index_quote,
        ltp_equity_and_index,
        payload_variants,
        quote_partial_zeros,
        quote_touchline_and_root,
        reconnect_replays,
        text_1105,
        unsubscribe_keeps_the_others,
    ]
);
