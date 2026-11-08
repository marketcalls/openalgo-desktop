//! Sahi: the protobuf market data stream (`brokers::sahi::streaming::SahiFeed`).
//! Sahi streams no order updates, on the web or here.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::sahi::streaming::SahiFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(SahiFeed::new("t".into(), env.symbols(), env.url("main")))
}

corpus_cases!(
    "sahi",
    feeds,
    [
        ltp_mode,
        quote_mode,
        depth_mode,
        index_quote,
        derivative_oi,
        reconnect_resubscribes,
        unsupported_exchange,
    ]
);
