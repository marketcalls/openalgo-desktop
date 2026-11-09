//! Firstock: one JSON feed for every mode (`brokers::firstock::streaming::
//! FirstockFeed`), on the path the live socket has (`/V2/ws`). The web
//! streams no order updates for Firstock (its handler only logs them), so
//! no order socket.
//!
//! The feed is the one a session gets (`FirstockBroker::create_feed`, its
//! user id and jKey from the session), and the broker wires the bridge as a
//! session does (`Feeds::with_broker`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::firstock::FirstockBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    let broker = Arc::new(FirstockBroker::with_urls(
        env.symbols(),
        env.http_url(),
        format!("{}/V2/ws", env.url("main")),
    ));
    let auth = AuthToken::new("AB1234:::jk");
    let feed = broker.create_feed(&auth).expect("Firstock's market feed");
    Feeds::boxed(feed).with_broker(broker, &auth)
}

corpus_cases!(
    "firstock",
    feeds,
    [
        quote_before_the_login_answer,
        ltp_then_quote,
        price_without_its_time,
        two_clients_share_a_quote,
        depth_future_book_clear,
        index_quote,
        reconnect_subscribes_again,
    ]
);
