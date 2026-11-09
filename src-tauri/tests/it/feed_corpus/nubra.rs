//! Nubra: the market feed (`NubraFeed`) and the order socket
//! (`NubraOrderFeed`, behind the loopback relay), built by `NubraBroker` on
//! the fake broker API: `GET /userinfo` names the fake order socket, as
//! Nubra names its own.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::nubra::NubraBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use serde_json::json;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    env.serve_json(
        "/userinfo",
        json!({"env_info": {"user_ws_url": env.url("orders")}}),
    );
    let broker = Arc::new(NubraBroker::with_urls(
        env.symbols(),
        env.http_url(),
        env.url("main"),
    ));
    let auth = AuthToken::new("nubra-corpus-session");
    Feeds::boxed(broker.create_feed(&auth).expect("the Nubra market feed"))
        .with_order_feed(
            broker
                .create_order_feed(&auth)
                .expect("the Nubra order feed"),
        )
        .with_broker(broker.clone(), &auth)
}

corpus_cases!(
    "nubra",
    feeds,
    [
        index_quote_ltp_bucket,
        equity_depth_orderbook,
        order_lifecycle,
        quote_and_depth_book_clear,
        one_sided_book_clear,
        book_before_price,
        book_first_after_reconnect,
        equity_quote_and_ltp
    ]
);
