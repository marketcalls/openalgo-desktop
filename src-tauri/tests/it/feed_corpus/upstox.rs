//! Upstox: Market Data Feed V3 (`UpstoxFeed`) and the portfolio order
//! stream (`UpstoxOrderFeed`), built by `UpstoxBroker` on the fake broker
//! API: each authorize call before a connect hands out the fake socket's
//! address, as Upstox hands out a single-use signed one.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::upstox::{UpstoxBroker, Urls};
use openalgo_desktop_lib::brokers::Broker;
use serde_json::json;
use std::sync::Arc;

fn authorized(url: String) -> serde_json::Value {
    json!({"status": "success", "data": {"authorized_redirect_uri": url}})
}

fn feeds(env: &Env) -> Feeds {
    env.serve_json(
        "/v3/feed/market-data-feed/authorize",
        authorized(env.url("main")),
    );
    env.serve_json(
        "/v2/feed/portfolio-stream-feed/authorize",
        authorized(env.url("orders")),
    );
    let broker = Arc::new(UpstoxBroker::with_urls(
        env.symbols(),
        Urls {
            api: env.http_url(),
            ..Urls::production()
        },
    ));
    let auth = AuthToken::new("upstox-corpus-session");
    Feeds::boxed(broker.create_feed(&auth).expect("the Upstox market feed"))
        .with_order_feed(
            broker
                .create_order_feed(&auth)
                .expect("the Upstox order feed"),
        )
        .with_broker(broker.clone(), &auth)
}

corpus_cases!(
    "upstox",
    feeds,
    [
        full_incremental_merge,
        ltpc_ltp_and_index_quote,
        auction_cas_fields,
        reconnect_fresh_authorize,
        order_lifecycle,
    ]
);
