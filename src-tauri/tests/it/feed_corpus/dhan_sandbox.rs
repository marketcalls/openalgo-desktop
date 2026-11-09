//! Dhan Sandbox: the simulated feed (`DhanSandboxFeed`), its tick frames
//! pushed by the fake broker in place of the loopback ticker, its
//! generator seeded so a run repeats.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::dhan_sandbox::feed::DhanSandboxFeed;

fn feeds(env: &Env) -> Feeds {
    Feeds::market(DhanSandboxFeed::seeded(
        &env.url("main"),
        env.symbols(),
        107,
    ))
}

corpus_cases!(
    "dhan_sandbox",
    feeds,
    [simulated_equity_modes, simulated_option_quote]
);
