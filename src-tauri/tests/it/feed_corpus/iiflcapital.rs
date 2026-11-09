//! IIFL Capital: the market data feed (`IiflFeed`) and the order and trade
//! updates (`IiflOrderFeed`), each reached through the loopback MQTT relay.
//! The fake broker plays the relay's side (`with_relay_url`): its ready
//! frame, the PUBLISH frames wrapping IIFL's own payloads, and the sub and
//! unsub frames the feed sends it. The session's broker is passed as
//! `broker_runtime` passes it (its refusals, depth levels and order
//! completion); its own feeds start the relay against a real MQTT broker,
//! which the corpus does not run.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::iiflcapital::streaming::{IiflFeed, IiflOrderFeed};
use openalgo_desktop_lib::brokers::iiflcapital::IiflCapitalBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use std::sync::Arc;

/// A session whose JWT payload names a `preferred_username`, the one claim
/// the feeds read; put together here so no token-shaped literal is
/// committed (`{"alg":"none"}`, `{"preferred_username":"CL101"}`).
fn session() -> String {
    [
        "eyJhbGciOiJub25lIn0",
        "eyJwcmVmZXJyZWRfdXNlcm5hbWUiOiJDTDEwMSJ9",
        "c2ln",
    ]
    .join(".")
}

fn feeds(env: &Env) -> Feeds {
    Feeds::market(
        IiflFeed::with_relay_url(&env.url("main"), &session())
            .expect("the fake session names a user"),
    )
    .with_orders(
        IiflOrderFeed::with_relay_url(&env.url("orders"), &session(), env.symbols())
            .expect("the fake session names a user"),
    )
    .with_broker(
        Arc::new(IiflCapitalBroker::with_base_url(
            env.symbols(),
            env.http_url(),
        )),
        &AuthToken::new(session()),
    )
}

corpus_cases!(
    "iiflcapital",
    feeds,
    [
        ltp_mode,
        quote_mode,
        full_depth,
        index_quote,
        future_oi_and_mode_change,
        reconnect_forgets_oi,
        order_lifecycle,
        order_token_on_two_exchanges,
        quote_untimed_packet,
    ]
);
