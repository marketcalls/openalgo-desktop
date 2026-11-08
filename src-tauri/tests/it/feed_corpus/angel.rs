//! Angel One: SmartStream (`brokers::angel::streaming::AngelFeed`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::angel::streaming::AngelFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(AngelFeed::new(
        &env.url("main"),
        "jwt",
        "key",
        "client",
        "feed",
        env.symbols(),
    ))
}

corpus_cases!("angel", feeds, [ltp_mode, quote_mode, snap_quote_depth]);
