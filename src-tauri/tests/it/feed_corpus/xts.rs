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
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

/// The broker login's interactive token, and the stored market data token
/// and user id the feed connects with (the socket address carries both).
const INTERACTIVE: &str = "itok";
const TOKEN: &str = "tok";
const USER: &str = "U1";

pub(super) fn feeds(cfg: &'static XtsConfig, env: &Env) -> Feeds {
    let broker = Arc::new(
        XtsBroker::with_base_url(cfg, env.symbols(), env.http_url())
            .with_socket_base(env.url("main")),
    );
    let auth = AuthToken::new(INTERACTIVE)
        .with_feed(Some(TOKEN))
        .with_user_id(USER);
    let feed = broker.create_feed(&auth).expect("the XTS market feed");
    Feeds::boxed(feed).with_broker(broker, &auth)
}
