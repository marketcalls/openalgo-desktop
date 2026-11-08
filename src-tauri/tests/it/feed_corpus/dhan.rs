//! Dhan: the market feed (`DhanFeed`) and the order-update socket
//! (`DhanOrderFeed`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::dhan::streaming::{DhanFeed, DhanOrderFeed};

fn feeds(env: &Env) -> Feeds {
    Feeds::market(DhanFeed::with_url(&env.url("main"), "t", "c")).with_orders(
        DhanOrderFeed::with_url(&env.url("orders"), "t", "c", env.symbols()),
    )
}

corpus_cases!(
    "dhan",
    feeds,
    [
        quote_previous_close,
        quote_before_previous_close,
        full_depth,
        reconnect_forgets_previous_close,
        order_lifecycle,
    ]
);
