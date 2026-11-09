//! The feed corpus harness (audit C09). A case's raw broker frames and its
//! subscription script go through the broker's real decoder (its
//! `BrokerFeed`), the real `WebSocketManager`, `BrokerBridge` and 8765
//! feed server, and every frame a client receives is compared with the
//! case's expected frame exactly. The case format is
//! `tests/fixtures/feed_corpus/README.md`.
//!
//! * [`case`]: a case as it is on disk, read and checked.
//! * [`upstream`]: the fake broker socket a feed connects to.
//! * [`http`]: the fake broker REST API (an authorize answer, an order
//!   book a poller reads, REST subscriptions).
//! * [`world`]: the production pieces, wired as a broker session wires them.
//! * [`run`]: the steps in order, and what may not be left over.
//! * [`compare`]: exact comparison of a frame, receipt times and random
//!   values included.
//! * [`invariants`]: what must hold of a value drawn at random.
//! * [`web`]: the expected frames against the web's output (`web.json`).

mod approvals;
mod case;
mod compare;
mod deliveries;
mod http;
mod invariants;
mod json_text;
mod run;
mod upstream;
mod web;
mod world;

pub(crate) use approvals::registry_problems;
pub(crate) use case::{corpus_root, deviation_problems, read_json, Case};
pub(crate) use run::run;
pub(crate) use world::{Env, Feeds};

/// One test per case folder of a broker, and the list the inventory
/// checks the folders against:
///
/// ```ignore
/// corpus_cases!("zerodha", feeds, [quote_mode, ltp_mode]);
/// ```
///
/// where `feeds` builds the broker's feeds for a case
/// (`fn(&Env) -> Feeds`) and each name is a folder under
/// `tests/fixtures/feed_corpus/zerodha/`.
macro_rules! corpus_cases {
    ($broker:literal, $feeds:path, [$($case:ident),+ $(,)?]) => {
        /// The case folders this module runs.
        pub(crate) const CASES: &[&str] = &[$(stringify!($case)),+];
        $(
            // One thread, so `World::drain` can run the pipeline dry by
            // turns of the scheduler rather than by a guess at the time.
            #[tokio::test]
            async fn $case() {
                $crate::feed_corpus::harness::run($broker, stringify!($case), $feeds).await;
            }
        )+
    };
}
pub(crate) use corpus_cases;
