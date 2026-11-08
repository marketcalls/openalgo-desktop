//! HDFC Securities (InvestRight): the market data feed
//! (`brokers::hdfcsecurities::streaming::HdfcSecuritiesFeed`), HDFC Sky's
//! sibling protocol. No order-update stream, here or on the web.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::hdfcsecurities::streaming::HdfcSecuritiesFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(HdfcSecuritiesFeed::new(
        &env.url("main"),
        "k",
        "t",
        env.symbols(),
    ))
}

corpus_cases!(
    "hdfcsecurities",
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
        in_band_auth_error,
    ]
);
