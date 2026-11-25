//! Pocketful: the market data socket (`PocketfulFeed`) and, on a socket of
//! its own, the order updates (`PocketfulOrderFeed`, web
//! `pocketful_order_adapter.py`), both on Pocketful's SDK address.
//!
//! Each is the one a session gets: the market feed from
//! `PocketfulBroker::create_feed`, the order feed from `create_order_feed`
//! (a broker whose feed host is the fake `orders` socket), on the session's
//! token and the client id stored with it.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::pocketful::PocketfulBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let at = |ws: String| {
        Arc::new(PocketfulBroker::with_base_url(
            env.symbols(),
            env.http_url(),
            ws,
            env.http_url(),
        ))
    };
    let broker = at(env.url("main"));
    let auth = AuthToken::new("t").with_user_id("c");
    let feed = broker.create_feed(&auth).expect("Pocketful's market feed");
    let feeds = Feeds::boxed(feed).with_broker(broker, &auth);
    if !env.orders() {
        return feeds;
    }
    let orders = at(env.url("orders"))
        .create_order_feed(&auth)
        .expect("Pocketful's order feed");
    feeds.with_order_feed(orders)
}

corpus_cases!(
    "pocketful",
    feeds,
    [
        ltp_compact,
        quote_detailed,
        quote_without_a_price,
        depth_snapquote,
        depth_20_refused_on_the_web,
        index_quote,
        future_quote_oi,
        reconnect_replays,
        two_clients_ltp_and_quote,
        snapquote_average_beside_a_trade_price,
        quote_untimed_packet,
        order_stream,
    ]
);
