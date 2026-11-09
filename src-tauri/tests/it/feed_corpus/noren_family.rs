//! The Noren family's feeds as a session builds them: the member's
//! `NorenBroker` on the fake hosts makes the market feed itself
//! (`Broker::create_feed` from the session's `uid:::token`), and the
//! harness wires the broker into the bridge and the order relay as
//! `broker_runtime` does (`Feeds::with_broker`: depth levels, exchange
//! refusals, presubscribe, order completion). Order updates ride the
//! market socket.

use super::harness::{Env, Feeds};
use openalgo_desktop_lib::brokers::families::noren::{NorenBroker, NorenConfig, NorenEndpoints};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use std::sync::Arc;

/// The session's stored token: trading user `U1`, access token `tok`.
const SESSION: &str = "U1:::tok";

pub(crate) fn feeds(env: &Env, cfg: &'static NorenConfig) -> Feeds {
    let endpoints = NorenEndpoints::rebased(cfg, &env.http_url(), &env.url("main"));
    let broker = NorenBroker::with_endpoints(cfg, env.symbols(), endpoints);
    let auth = AuthToken::new(SESSION);
    let feed = broker
        .create_feed(&auth)
        .expect("the member's market data feed is made from the session");
    Feeds::boxed(feed).with_broker(Arc::new(broker), &auth)
}
