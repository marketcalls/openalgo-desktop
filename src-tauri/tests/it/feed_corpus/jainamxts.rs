//! Jainam XTS: the XTS market data feed with its own configuration
//! (`brokers::jainamxts::CONFIG`; the family's feed in [`super::xts`]).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::jainamxts::CONFIG;

fn feeds(env: &Env) -> Feeds {
    super::xts::feeds(&CONFIG, env)
}

corpus_cases!(
    "jainamxts",
    feeds,
    [
        depth_five_and_clear,
        depth_five_beside_twenty,
        depth_thirty_and_fifty_refused,
        depth_twenty,
        depth_zero_slots_kept,
        event_name_supplies_the_code,
        future_depth_oi,
        index_quote_root,
        index_quote_touchline,
        ltp_equity_and_index,
        oi_event_1510_kept,
        quote_partial_keeps_the_quote,
        quote_root,
        quote_touchline,
        reconnect_replays,
        server_disconnect_reconnects,
        subscribe_refused_by_the_broker,
        text_1105,
        unsubscribe_keeps_the_others,
    ]
);
