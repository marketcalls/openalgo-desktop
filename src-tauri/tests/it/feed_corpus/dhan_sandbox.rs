//! Dhan Sandbox: the simulated feed (`DhanSandboxFeed`), its tick frames
//! pushed by the fake broker in place of the loopback ticker, its
//! generator seeded so a run repeats.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::dhan_sandbox::{self, feed::DhanSandboxFeed};
use openalgo_desktop_lib::brokers::types::AuthToken;
use std::sync::Arc;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(DhanSandboxFeed::seeded(
        &env.url("main"),
        env.symbols(),
        107,
    ))
    .with_broker(
        Arc::new(dhan_sandbox::broker(env.symbols())),
        &AuthToken::new("sandbox-secret"),
    )
}

corpus_cases!(
    "dhan_sandbox",
    feeds,
    [simulated_equity_modes, simulated_option_quote]
);
