//! HDFC Securities (InvestRight): the market data feed
//! (`brokers::hdfcsecurities::streaming::HdfcSecuritiesFeed`), HDFC Sky's
//! sibling protocol, built by the broker as a session builds it
//! (`create_feed` from the stored `api_key:token`), with the broker wired
//! into the bridge (its depth levels and refusals). No order-update
//! stream, here or on the web.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::hdfcsecurities::HdfcSecuritiesBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(HdfcSecuritiesBroker::with_base_url(
        env.symbols(),
        env.http_url(),
        format!("{}/master", env.http_url()),
        env.url("main"),
    ));
    // API key `k`, access token `t`: the socket's query names both.
    let auth = AuthToken::new("k:t");
    let feed = broker
        .create_feed(&auth)
        .expect("HDFC Securities' market feed");
    Feeds::boxed(feed).with_broker(broker, &auth)
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
        no_trade_yet,
        in_band_auth_error,
        empty_book_first,
        empty_book_between_books,
        empty_book_quote_and_depth,
    ]
);
