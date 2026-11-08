//! Paytm Money: the market data socket (`PaytmFeed`), authenticated by the
//! query (`x_jwt_token`). The web streams no Paytm order updates.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::paytm::streaming::PaytmFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(PaytmFeed::with_url(&env.url("main"), "pat", env.symbols()))
}

corpus_cases!(
    "paytm",
    feeds,
    [
        ltp_mode,
        quote_mode,
        full_depth,
        index_ltp,
        index_quote,
        index_full,
        future_quote_then_depth,
        reconnect_replays,
        depth_20_refused_by_the_web,
    ]
);
