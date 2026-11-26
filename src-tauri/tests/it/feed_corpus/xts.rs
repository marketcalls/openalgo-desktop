//! The Symphony XTS family's market data feed (`XtsFeed`), shared by the
//! seven members' modules: each passes its own `XtsConfig`, so a case runs
//! the member's socket path, broadcast mode, REST root and message codes.
//!
//! The feed is the one a session gets (`XtsBroker::create_feed`, from the
//! stored feed token and user id), and the broker wires the bridge as a
//! session does (`Feeds::with_broker`: its depth levels and refusals).
//!
//! An XTS feed speaks Socket.IO on the socket (a case pushes the Engine.IO
//! open, takes the feed's `40` and pushes the connect ack) and subscribes
//! over REST: `POST` and `PUT {subscription_path}/instruments/subscription`
//! on the fake broker API, which each case answers and records (`case.json`
//! `http`), so its `http_receives` steps compare every subscription body
//! and wait for it before the next event is pushed.

use super::harness::{Env, Feeds};
use openalgo_desktop_lib::brokers::families::xts::{XtsBroker, XtsConfig};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use std::sync::Arc;

/// The broker login's interactive token, and the stored market data token
/// and user id the feed connects with (the socket address carries both).
const INTERACTIVE: &str = "itok";
const TOKEN: &str = "tok";
const USER: &str = "U1";

pub(super) fn feeds(cfg: &'static XtsConfig, env: &Env) -> Feeds {
    feeds_with(cfg, env, None, TOKEN)
}

/// The feed of a session signed in with its market data keys, which the
/// adapter keeps in memory (`Broker::restore_session`, as a resume does),
/// and `market_token` stored at sign-in. 5paisa XTS's feed connects with
/// the stored session and never logs in while one is stored (web #2484),
/// so a login would reach the fake API unanswered and fail the case.
pub(super) fn feeds_signed_in(cfg: &'static XtsConfig, env: &Env, market_token: &str) -> Feeds {
    let keys = BrokerCredentials {
        api_key_market: Some("mkey".into()),
        api_secret_market: Some("msecret".into()),
        ..Default::default()
    };
    feeds_with(cfg, env, Some(keys), market_token)
}

fn feeds_with(
    cfg: &'static XtsConfig,
    env: &Env,
    keys: Option<BrokerCredentials>,
    market_token: &str,
) -> Feeds {
    let broker = Arc::new(
        XtsBroker::with_base_url(cfg, env.symbols(), env.http_url())
            .with_socket_base(env.url("main")),
    );
    if let Some(keys) = &keys {
        broker.restore_session(keys);
    }
    let auth = AuthToken::new(INTERACTIVE)
        .with_feed(Some(market_token))
        .with_user_id(USER);
    let feed = broker.create_feed(&auth).expect("the XTS market feed");
    Feeds::boxed(feed).with_broker(broker, &auth)
}
