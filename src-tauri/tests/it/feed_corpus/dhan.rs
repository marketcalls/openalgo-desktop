//! Dhan: the market feed (`DhanFeed`), the 20-level depth socket
//! (`Dhan20DepthFeed`) and the order-update socket (`DhanOrderFeed`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::dhan::streaming::{Dhan20DepthFeed, DhanFeed, DhanOrderFeed};

fn feeds(env: &Env) -> Feeds {
    Feeds::market(DhanFeed::with_url(&env.url("main"), "t", "c"))
        .with_depth(20, Dhan20DepthFeed::with_url(&env.url("depth"), "t", "c"))
        .with_orders(DhanOrderFeed::with_url(
            &env.url("orders"),
            "t",
            "c",
            env.symbols(),
        ))
}

corpus_cases!(
    "dhan",
    feeds,
    [
        quote_previous_close,
        quote_before_previous_close,
        full_depth,
        depth_20_before_and_after_the_quote,
        reconnect_forgets_previous_close,
        order_lifecycle,
    ]
);
