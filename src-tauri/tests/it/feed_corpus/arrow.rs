//! Arrow: the market data socket (`ArrowFeed`) and the order-update socket
//! (`ArrowOrderFeed`), both authenticated by the query (`appID`, `token`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::arrow::streaming::{ArrowFeed, ArrowOrderFeed};

fn feeds(env: &Env) -> Feeds {
    Feeds::market(ArrowFeed::new(&env.url("main"), "app", "jwt")).with_orders(ArrowOrderFeed::new(
        &env.url("orders"),
        "app",
        "jwt",
        env.symbols(),
    ))
}

corpus_cases!(
    "arrow",
    feeds,
    [
        ltp_mode,
        quote_mode,
        full_depth,
        index_ltp_quote,
        future_quote_then_depth,
        reconnect_replays,
        order_lifecycle,
        order_stop_loss_margin,
        order_symbol_by_token,
    ]
);
