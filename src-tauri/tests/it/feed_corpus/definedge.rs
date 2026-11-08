//! Definedge: the NorenWSTRTP market data feed (`DefinedgeFeed`: touchline
//! and depth on one socket, after the `ck` connect acknowledgement) and
//! the order-update connection (`DefinedgeOrderFeed`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::definedge::streaming::{DefinedgeFeed, DefinedgeOrderFeed};

fn feeds(env: &Env) -> Feeds {
    Feeds::market(DefinedgeFeed::new(&env.url("main"), "u", "t")).with_orders(
        DefinedgeOrderFeed::new(&env.url("orders"), "u", "t", env.symbols()),
    )
}

corpus_cases!(
    "definedge",
    feeds,
    [
        ltp_mode,
        quote_mode,
        depth_mode,
        depth_ack_without_ohlc,
        index_quote,
        future_quote_and_depth_oi,
        mode_transitions,
        reconnect_resubscribes,
        order_updates,
    ]
);
