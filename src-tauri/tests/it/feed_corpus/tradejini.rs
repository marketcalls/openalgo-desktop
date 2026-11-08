//! Tradejini: the NxtradStream binary feed
//! (`brokers::tradejini::streaming::TradejiniFeed`), L1 quotes and L5
//! books on one socket. The web streams no Tradejini order updates
//! (`services/order_update_service.py` has no Tradejini adapter).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::tradejini::streaming::TradejiniFeed;

fn feeds(env: &Env) -> Feeds {
    // The stream's own path (`streaming::STREAM_URL`) on the fake socket.
    let base = format!("{}/v2.1/stream", env.url("main"));
    Feeds::market(TradejiniFeed::new(&base, "key", "tok"))
}

corpus_cases!(
    "tradejini",
    feeds,
    [
        quote_mode,
        ltp_mode,
        depth_waits_for_the_quote,
        depth_book_update_and_clear,
        depth_to_quote_and_back,
        reconnect_drops_the_book,
        index_quote_and_ltp,
        future_quote_oi,
        future_depth_oi,
    ]
);
