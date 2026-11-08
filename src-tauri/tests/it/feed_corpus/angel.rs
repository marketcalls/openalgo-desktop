//! Angel One: SmartStream (`brokers::angel::streaming::AngelFeed`) and the
//! order-status socket (`AngelOrderFeed`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::angel::streaming::{AngelFeed, AngelOrderFeed};

fn feeds(env: &Env) -> Feeds {
    Feeds::market(AngelFeed::new(
        &env.url("main"),
        "jwt",
        "key",
        "client",
        "feed",
        env.symbols(),
    ))
    .with_orders(AngelOrderFeed::new(
        &env.url("orders"),
        "jwt",
        env.symbols(),
    ))
}

corpus_cases!(
    "angel",
    feeds,
    [
        ltp_mode,
        quote_mode,
        snap_quote_depth,
        index_quote_reconnect,
        future_snap_oi,
        quote_depth_mode_change,
        order_lifecycle,
    ]
);
