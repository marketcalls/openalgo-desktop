//! HDFC Sky: the market data feed (`brokers::hdfcsky::streaming::HdfcSkyFeed`),
//! protobuf over a query-authenticated socket. HDFC Sky has no order-update
//! stream, here or on the web.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::hdfcsky::streaming::HdfcSkyFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(HdfcSkyFeed::new(&env.url("main"), "k", "t"))
}

corpus_cases!(
    "hdfcsky",
    feeds,
    [
        ltp_mode,
        quote_mode,
        depth_update_and_book_clear,
        derivative_oi_and_circuit_partials,
        index_quote_and_depth,
        reconnect_drops_the_snapshot,
        two_clients_mode_change,
        one_token_on_two_exchanges,
        no_trade_yet,
    ]
);
