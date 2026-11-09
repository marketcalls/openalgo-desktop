//! 5paisa XTS: the XTS market data feed with its own configuration
//! (`brokers::fivepaisaxts::CONFIG`; the family's feed in [`super::xts`]).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::fivepaisaxts::CONFIG;

fn feeds(env: &Env) -> Feeds {
    super::xts::feeds(&CONFIG, env)
}

corpus_cases!(
    "fivepaisaxts",
    feeds,
    [
        depth_five_and_clear,
        depth_five_beside_twenty,
        depth_thirty_and_fifty_refused,
        depth_twenty,
        depth_zero_slots_kept,
        future_depth_oi,
        index_quote_root,
        index_quote_touchline,
        ltp_equity_and_index,
        quote_partial_keeps_the_quote,
        quote_root,
        quote_touchline,
        reconnect_replays,
        server_disconnect_reconnects,
        text_1105,
        unsubscribe_keeps_the_others,
    ]
);
