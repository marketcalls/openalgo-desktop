//! TradeSmart: the Noren market data socket (`NorenFeed` on TradeSmart's
//! config). TradeSmart pushes the `om` order updates on it once the login
//! is accepted, without `{"t":"o"}`. The session's `uid:::token` is split
//! as `create_feed` splits it (`noren::transport::session`), so the feed
//! gets the bare user id and token.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::families::noren::streaming::NorenFeed;
use openalgo_desktop_lib::brokers::tradesmart;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(NorenFeed::new(
        tradesmart::config(),
        &env.url("main"),
        "U1",
        "tok",
        env.symbols(),
    ))
}

corpus_cases!(
    "tradesmart",
    feeds,
    [
        quote_snapshot_and_update,
        index_ltp_and_quote,
        depth_future_oi,
        reconnect_replays,
        order_updates,
        frame_without_exchange,
    ]
);
