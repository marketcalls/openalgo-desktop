//! 5paisa (the native feed, `brokers::fivepaisa::streaming::FivepaisaFeed`;
//! not 5paisa XTS). Order updates are polled from the REST order book, so
//! no order socket is built here.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::fivepaisa::streaming::FivepaisaFeed;

fn feeds(env: &Env) -> Feeds {
    // The production path (`feeds/api/chat`) on the fake broker's address.
    let host = format!("{}/feeds/api/chat", env.url("main"));
    Feeds::market(FivepaisaFeed::new(&host, "t", "c"))
}

corpus_cases!(
    "fivepaisa",
    feeds,
    [
        ltp_mode,
        quote_mode,
        depth_after_quote,
        depth_before_quote,
        same_token_two_exchanges,
        future_mode_change,
        index_quote,
        reconnect_after_held_prices,
    ]
);
