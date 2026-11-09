//! Delta Exchange: the public market-data socket (`DeltaFeed`), which needs
//! no login. Delta streams no order updates to feed clients (see the
//! broker's `broker.json`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::deltaexchange::streaming::DeltaFeed;
use openalgo_desktop_lib::brokers::deltaexchange::DeltaBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(DeltaFeed::new(&env.url("main"))).with_broker(
        Arc::new(DeltaBroker::new(env.symbols())),
        &AuthToken::new("delta-corpus-session"),
    )
}

corpus_cases!(
    "deltaexchange",
    feeds,
    [
        ticker_quote_ltp_mode_change,
        book_then_ticker_depth,
        reconnect_then_mark_only,
        reconnect_then_book_before_price,
    ]
);
