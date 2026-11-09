//! AliceBlue: the Noren market socket and the Order Status Feed, built by
//! `AliceBlueBroker::create_feed` and `create_order_feed` as the app builds
//! them, each through the loopback relay. Before every connect the relay
//! asks AliceBlue's REST API for a socket session (`createWsSess`) or an
//! order token (`createWsToken`); the fake broker API answers as AliceBlue
//! does. The broker wires the bridge as a session does
//! (`Feeds::with_broker`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::aliceblue::{AliceBlueBroker, Endpoints};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use serde_json::json;
use std::sync::Arc;

/// A fake JWT (placeholder signature) and the client code it belongs to.
const JWT: &str = "header.payload.signature";
const UCC: &str = "AB1";

fn feeds(env: &Env) -> Feeds {
    for path in [
        "/open-api/od/v1/profile/invalidateWsSess",
        "/open-api/od/v1/profile/createWsSess",
    ] {
        env.serve_json(path, json!({"status": "Ok", "result": []}));
    }
    env.serve_json(
        "/open-api/order-notify/ws/createWsToken",
        json!({"status": "Ok", "result": [{"orderToken": "order-token-test"}]}),
    );
    let broker = Arc::new(AliceBlueBroker::with_endpoints(
        env.symbols(),
        Endpoints {
            base: env.http_url(),
            ws: vec![format!("{}/NorenWS/", env.url("main"))],
            order_ws: format!("{}/open-api/order-notify/websocket", env.url("orders")),
            ..Endpoints::default()
        },
    ));
    let auth = AuthToken::new(JWT).with_user_id(UCC);
    let market = broker.create_feed(&auth).expect("AliceBlue's market feed");
    let orders = broker
        .create_order_feed(&auth)
        .expect("AliceBlue's order feed");
    Feeds::boxed(market)
        .with_order_feed(orders)
        .with_broker(broker, &auth)
}

corpus_cases!(
    "aliceblue",
    feeds,
    [
        quote_snapshot_then_delta,
        absent_fields_and_a_price_without_time,
        two_clients_share_a_quote,
        ltp_then_quote,
        depth_future_deltas,
        depth_bid_side_zeroed,
        index_quote,
        reconnect_logs_in_again,
        order_lifecycle,
    ]
);
