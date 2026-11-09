//! Dhan: the market feed (`DhanFeed`), the 20-level depth socket
//! (`Dhan20DepthFeed`) and the order-update socket (`DhanOrderFeed`), with
//! the session's `DhanBroker` for its depth levels and exchange refusals.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::dhan::streaming::{Dhan20DepthFeed, DhanFeed, DhanOrderFeed};
use openalgo_desktop_lib::brokers::dhan::DhanBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(DhanBroker::new(env.symbols()));
    Feeds::market(DhanFeed::with_url(&env.url("main"), "t", "c"))
        .with_depth(20, Dhan20DepthFeed::with_url(&env.url("depth"), "t", "c"))
        .with_orders(DhanOrderFeed::with_url(
            &env.url("orders"),
            "t",
            "c",
            env.symbols(),
        ))
        .with_broker(broker, &AuthToken::new("c:::t"))
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
        ltp_ticker,
        index_quote,
        future_depth_oi,
        quote_to_depth_mode_change,
        deliberate_disconnect,
        exchange_not_supported,
        decimal_prices,
        oi_high_low_persist,
        depth_20_first_book_beside_5,
        depth_20_feed_not_made,
        depth_20_socket_refused,
        depth_20_silent_falls_back,
    ]
);
