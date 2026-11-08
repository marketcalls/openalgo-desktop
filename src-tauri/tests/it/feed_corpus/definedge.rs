//! Definedge: the NorenWSTRTP market data feed (`DefinedgeFeed`: touchline
//! and depth on one socket, after the `ck` connect acknowledgement) and
//! the order-update connection (`DefinedgeOrderFeed`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::definedge::streaming::{DefinedgeFeed, DefinedgeOrderFeed};
use openalgo_desktop_lib::brokers::definedge::{DefinedgeBroker, Endpoints};
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    // The broker wires the bridge as a session does (its depth levels and
    // exchange refusals). Its feeds share one socket address, so they are
    // built here, each on its own fake socket.
    let http = env.http_url();
    let broker = DefinedgeBroker::with_endpoints(
        env.symbols(),
        Endpoints {
            signin: http.clone(),
            trade: http.clone(),
            data: http.clone(),
            master: http,
            ws: env.url("main"),
        },
    );
    Feeds::market(DefinedgeFeed::new(&env.url("main"), "u", "t"))
        .with_orders(DefinedgeOrderFeed::new(
            &env.url("orders"),
            "u",
            "t",
            env.symbols(),
        ))
        .with_broker(Arc::new(broker))
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
