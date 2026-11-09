//! Rupeezy: the Vortex price socket (`RupeezyFeed`), authenticated by the
//! query (`auth_token`). The web streams no Rupeezy order updates.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::rupeezy::streaming::RupeezyFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(RupeezyFeed::new(&env.url("main"), "tok"))
}

corpus_cases!(
    "rupeezy",
    feeds,
    [
        ltp_mode,
        quote_mode,
        full_depth,
        index_quote,
        future_ltp_and_depth,
        reconnect_replays,
    ]
);
