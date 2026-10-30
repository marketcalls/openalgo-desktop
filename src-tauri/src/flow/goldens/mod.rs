//! Behaviour goldens: OpenAlgo web's own executor, recorded running every
//! node type and whole workflows against a scripted client
//! (`tests/fixtures/web/flow/goldens/`, format in its README), replayed here
//! through the desktop's node registry and walk.
//!
//! One test per family (`goldens_orders`, `goldens_data`, ...), so a lane
//! replays its own and a red family does not hide another's. A golden whose
//! node type no family registers yet is reported as "not built" (one line
//! per family), so this lands before the node lanes. Once a type is
//! registered, every one of its goldens must match: the same client calls
//! with the same payloads, the same result, variables, condition results,
//! run log, branch taken and failures; and for the nodes that read more
//! than the client, the same master-contract, feed and HTTP traffic
//! (`env.rs` builds the node services of `contract::env` from a golden's
//! `env`). A golden that needs something the harness cannot give a run yet
//! (a monitor-fired run, the app's store, the restacked validator, a feed
//! that raises) is reported as "needs <hook>"; whoever adds that hook to
//! `replay.rs` makes those goldens count. Run with `--nocapture` to see the
//! per-family lines.

mod compare;
mod env;
mod pyrepr;
mod replay;

use replay::Verdict;
use std::collections::BTreeMap;

#[derive(Default)]
struct Tally {
    total: usize,
    matched: usize,
    not_built: BTreeMap<String, usize>,
    needs: BTreeMap<String, usize>,
    evidence_only: usize,
    failed: Vec<(String, Vec<String>)>,
}

/// Replay one family's goldens and fail on any mismatch.
async fn replay_family(family: &str) {
    let goldens: Vec<replay::Golden> = replay::load_all()
        .into_iter()
        .filter(|g| g.family == family)
        .collect();
    assert!(!goldens.is_empty(), "no goldens found for {family}");
    let mut t = Tally::default();
    for g in &goldens {
        t.total += 1;
        match replay::replay(g).await {
            Verdict::Matched => t.matched += 1,
            Verdict::NotBuilt(ty) => *t.not_built.entry(ty).or_default() += 1,
            Verdict::NeedsHook(h) => *t.needs.entry(h.to_string()).or_default() += 1,
            Verdict::EvidenceOnly => t.evidence_only += 1,
            Verdict::Failed(d) => t.failed.push((g.name.clone(), d)),
        }
    }
    let not_built: usize = t.not_built.values().sum();
    eprintln!(
        "goldens {family}: {} total, replayed {}, matched {}, not built {not_built}, needs a hook {}, web evidence only {}",
        t.total,
        t.matched + t.failed.len(),
        t.matched,
        t.needs.values().sum::<usize>(),
        t.evidence_only
    );
    if not_built > 0 {
        let types: Vec<String> = t
            .not_built
            .iter()
            .map(|(k, v)| format!("{k} ({v})"))
            .collect();
        eprintln!("  not built yet: {}", types.join(", "));
    }
    if !t.needs.is_empty() {
        let hooks: Vec<String> = t.needs.iter().map(|(k, v)| format!("{k} ({v})")).collect();
        eprintln!("  needs a hook: {}", hooks.join(", "));
    }
    for (name, diffs) in &t.failed {
        eprintln!("  MISMATCH {name}");
        for d in diffs.iter().take(12) {
            eprintln!("    {d}");
        }
    }
    assert!(
        t.failed.is_empty(),
        "{} {family} golden(s) do not match the web; see the lines above",
        t.failed.len()
    );
}

#[tokio::test(start_paused = true)]
async fn goldens_orders() {
    replay_family("orders").await;
}

#[tokio::test(start_paused = true)]
async fn goldens_data() {
    replay_family("data").await;
}

#[tokio::test(start_paused = true)]
async fn goldens_logic() {
    replay_family("logic").await;
}

#[tokio::test(start_paused = true)]
async fn goldens_triggers() {
    replay_family("triggers").await;
}

#[tokio::test(start_paused = true)]
async fn goldens_core() {
    replay_family("core").await;
}

#[tokio::test(start_paused = true)]
async fn goldens_workflows() {
    replay_family("workflows").await;
}

/// Every golden file parses and belongs to a family a test replays.
#[test]
fn every_golden_is_replayed_by_a_family_test() {
    let all = replay::load_all();
    assert!(all.len() > 700, "goldens not found: {}", all.len());
    for g in &all {
        assert!(g.v.is_object(), "{} is not a JSON object", g.name);
        assert!(
            ["orders", "data", "logic", "triggers", "core", "workflows"]
                .contains(&g.family.as_str()),
            "{} is in a folder no test replays",
            g.name
        );
    }
}
