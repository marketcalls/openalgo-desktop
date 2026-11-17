//! Nuvama: the market data feed (`NuvamaFeed`) and the order updates
//! (`NuvamaOrderFeed`), each reached through the loopback line relay. The
//! fake broker plays the relay's side (`with_relay_url`): its ready frame,
//! the lines Nuvama sends as text frames, and the requests the feed sends
//! it. The relay's own batching (one whole-set request per streaming type
//! once the changes are quiet) is covered by the broker's unit and
//! integration tests (`tests/it/broker_nuvama/relay.rs`), not here.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::nuvama::session::NuvamaSession;
use openalgo_desktop_lib::brokers::nuvama::streaming::{NuvamaFeed, NuvamaOrderFeed};
use openalgo_desktop_lib::brokers::nuvama::{NuvamaBroker, NuvamaUrls};
use openalgo_desktop_lib::brokers::types::AuthToken;
use std::sync::Arc;

/// A fake session: the app id key `APPID`, the account `ACC01`.
const TOKEN: &str = "AUTH:::SOURCE:::APPID:::ACC01:::EQ:::KEY01";

fn feeds(env: &Env) -> Feeds {
    let session = NuvamaSession::parse(TOKEN).expect("a complete fake session");
    Feeds::market(NuvamaFeed::with_relay_url(&env.url("main"), &session))
        .with_orders(NuvamaOrderFeed::with_relay_url(
            &env.url("orders"),
            &session,
            env.symbols(),
        ))
        .with_broker(
            Arc::new(NuvamaBroker::with_base_url(
                env.symbols(),
                NuvamaUrls::rebased(&env.http_url(), 1),
            )),
            &AuthToken::new(TOKEN),
        )
}

corpus_cases!(
    "nuvama",
    feeds,
    [
        ltp_mode,
        quote_mode,
        full_depth,
        index_quote,
        option_oi_and_mode_change,
        reconnect_replays,
        order_lifecycle,
    ]
);
