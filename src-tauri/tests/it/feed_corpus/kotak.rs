//! Kotak Neo: the SFeed market feed and the realtime order socket, both
//! from the broker as a resumed session builds them (`create_feed`,
//! `create_order_feed`). The market feed looks its data centre's host up in
//! the feed-config service before it connects, as the web does
//! (`kotak_feed_config.py`), and the fake broker API answers with the fake
//! market socket. The order socket is the `/realtime` path on the session's
//! trading host (`baseUrl`), so the session names the fake order socket.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::kotak::KotakBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use serde_json::json;
use std::sync::Arc;

/// The feed-config service's path (`kotak::FEED_CONFIG_URL`).
const FEED_CONFIG: &str = "/5config/config";

/// Where the market feed goes when the config names no host or cannot be
/// read, in place of Kotak's own SFeed host: port 0 on loopback, which no
/// socket listens on, so a broken config route fails the case (its market
/// feed never connects) without leaving the machine. The fake market socket
/// would hide the break.
const NO_FEED_HOST: &str = "ws://127.0.0.1:0/apifeed";

/// The fake socket `name` as Kotak names a host: `https://` in the config
/// answer and in the session's `baseUrl`, dialled as `wss://` (here
/// `http://` and `ws://`).
fn host(env: &Env, name: &str) -> String {
    env.url(name).replacen("ws://", "http://", 1)
}

fn feeds(env: &Env) -> Feeds {
    // Data centre E43 streams from SFeed, as it does live
    // (`https://sfeed-e43.kotaksecurities.com/apifeed`).
    let endpoint = format!("{}/apifeed", host(env, "main"));
    env.serve_json(
        FEED_CONFIG,
        json!({"data": {"configs": {
            "E43_broadcast_source": "sh",
            "E43_sh_broadcast_endpoint": endpoint,
        }}}),
    );
    let broker = Arc::new(
        KotakBroker::with_urls(
            env.symbols(),
            env.http_url(),
            format!("{}{}", env.http_url(), FEED_CONFIG),
        )
        .with_feed_fallback(NO_FEED_HOST),
    );
    // A resumed session puts the UCC back from the saved keys; SFeed's
    // login carries it as `user`.
    broker.restore_session(&BrokerCredentials {
        api_key: "U".into(),
        ..BrokerCredentials::default()
    });
    // `token:::sid:::baseUrl:::accessToken:::dataCenter`.
    let auth = AuthToken::new(format!("t:::s:::{}:::a:::E43", host(env, "orders")));
    let market = broker.create_feed(&auth).expect("Kotak's market feed");
    let orders = broker
        .create_order_feed(&auth)
        .expect("Kotak's order-update socket");
    Feeds::boxed(market)
        .with_broker(broker, &auth)
        .with_order_feed(orders)
}

corpus_cases!(
    "kotak",
    feeds,
    [
        quote_mode,
        ltp_mode,
        first_mini_tick,
        depth_book_replaced,
        index_two_names_one_feed,
        index_depth,
        derivative_quote_and_depth,
        reconnect_clears_the_book,
        bcd_currency_quote,
        order_lifecycle,
        order_socket_never_acknowledged,
    ]
);
