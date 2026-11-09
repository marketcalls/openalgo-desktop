//! The Symphony XTS family's market data feed (`XtsFeed`), shared by the
//! seven members' modules: each passes its own `XtsConfig`, so a case runs
//! the member's socket path, broadcast mode, REST root and message codes.
//!
//! An XTS feed speaks Socket.IO on the socket (a case pushes the Engine.IO
//! open, takes the feed's `40` and pushes the connect ack) and subscribes
//! over REST: `POST` and `PUT {subscription_path}/instruments/subscription`
//! on the fake broker API, which each case answers and records (`case.json`
//! `http`), so its `http_receives` steps compare every subscription body
//! and wait for it before the next event is pushed.

use super::harness::{Env, Feeds};
use openalgo_desktop_lib::brokers::families::xts::streaming::{FeedSource, XtsFeed};
use openalgo_desktop_lib::brokers::families::xts::XtsConfig;
use std::time::Duration;

/// The stored market data token and user id the feed connects with; the
/// socket address carries both.
const TOKEN: &str = "tok";
const USER: &str = "U1";

pub(super) fn feeds(cfg: &'static XtsConfig, env: &Env) -> Feeds {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("an HTTP client");
    Feeds::market(XtsFeed::new(
        cfg,
        http,
        env.http_url(),
        Some(env.url("main")),
        FeedSource::stored(TOKEN, USER),
    ))
}
