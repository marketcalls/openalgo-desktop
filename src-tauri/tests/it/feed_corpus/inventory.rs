//! The corpus folder and the case lists agree: a case folder no test runs,
//! or a listed case with no folder, fails here; every case reads and
//! checks (its `case.json`, script and frame files) without a socket; and
//! every deviation of every case is sound, so the parity matrix that
//! counts them can be trusted.

use super::harness::{corpus_root, deviation_problems, read_json, registry_problems, Case};
use super::{BROKERS, WITHOUT_BROKER};
use openalgo_desktop_lib::brokers::catalog::ALL_BROKERS;
use std::collections::BTreeSet;

fn folders(dir: &std::path::Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {}", dir.display(), e))
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn every_case_folder_is_run_and_reads() {
    let root = corpus_root();
    let on_disk: BTreeSet<String> = folders(&root)
        .into_iter()
        .flat_map(|b| {
            let cases = folders(&root.join(&b));
            cases.into_iter().map(move |c| format!("{}/{}", b, c))
        })
        .collect();
    let listed: BTreeSet<String> = BROKERS
        .iter()
        .flat_map(|(b, cases)| cases.iter().map(move |c| format!("{}/{}", b, c)))
        .collect();
    let unlisted: Vec<&String> = on_disk.difference(&listed).collect();
    let missing: Vec<&String> = listed.difference(&on_disk).collect();
    assert!(
        unlisted.is_empty() && missing.is_empty(),
        "case folders no test runs: {:?}; listed cases with no folder: {:?}",
        unlisted,
        missing
    );
    for (broker, cases) in BROKERS {
        for case in *cases {
            Case::load(broker, case);
        }
    }
}

/// Every deviation on disk, listed or not, names its key once, a known
/// kind and a reason; all that fall short are listed together, where a
/// case's own load stops at its first.
#[test]
fn every_deviation_has_a_known_kind_and_a_reason() {
    let root = corpus_root();
    let mut problems = Vec::new();
    for broker in folders(&root) {
        for case in folders(&root.join(&broker)) {
            let path = root.join(&broker).join(&case).join("case.json");
            if !path.exists() {
                problems.push(format!("{}/{}: no case.json", broker, case));
                continue;
            }
            let case_json = read_json(&path);
            for p in deviation_problems(&broker, case_json.get("deviations")) {
                problems.push(format!("{}/{}: {}", broker, case, p));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "deviations the parity matrix cannot count:\n  {}",
        problems.join("\n  ")
    );
}

/// The check above finds each way a deviation falls short.
#[test]
fn an_unsound_deviation_is_named() {
    let devs = serde_json::json!([
        {"key": "ltt", "kind": "web-defect", "reason": "seconds, not ms", "approved": true},
        {"key": "oi", "kind": "guess", "reason": "?"},
        {"key": "ltt", "kind": "open", "reason": " "},
        {"kind": "open", "reason": "no key", "approved": "yes", "note": "x"}
    ]);
    let found = deviation_problems("zerodha", Some(&devs));
    let want = [
        "deviation \"oi\": kind \"guess\" is not one of",
        "deviation \"ltt\" gives no reason",
        "deviation \"ltt\" is listed twice",
        "deviation 4 names no \"key\"",
        "deviation 4: \"note\" is not a field of a deviation",
        "deviation 4: approved is true or false",
    ];
    for w in want {
        assert!(
            found.iter().any(|f| f.starts_with(w)),
            "{} not in {:?}",
            w,
            found
        );
    }
    assert_eq!(found.len(), want.len(), "{:?}", found);
}

/// Every rule a deviation may name says who decided it, when, where it is
/// written, and quotes it; an approval names one of them and never sits on
/// an open gap.
#[test]
fn approvals_name_complete_rules() {
    let problems = registry_problems();
    assert!(
        problems.is_empty(),
        "approvals.json:\n  {}",
        problems.join("\n  ")
    );
    let devs = serde_json::json!([
        {"key": "ltt", "kind": "web-defect", "reason": "seconds", "approval": "alias-unit"},
        {"key": "oi", "kind": "web-defect", "reason": "zero", "approval": "made-up"},
        {"key": "ltp", "kind": "open", "reason": "to fix", "approval": "alias-unit"}
    ]);
    let found = deviation_problems("zerodha", Some(&devs));
    assert_eq!(found.len(), 2, "{:?}", found);
    assert!(
        found[0].contains("approval \"made-up\" is no rule"),
        "{:?}",
        found
    );
    assert!(
        found[1].contains("an open gap is never approved"),
        "{:?}",
        found
    );
}

/// Random values are for a feed whose web output really is drawn at
/// random: on any other broker the kind would make a value comparison a
/// type check.
#[test]
fn only_a_synthetic_feed_may_declare_random_values() {
    let devs = serde_json::json!([
        {"key": "ltp", "kind": "random-values", "reason": "drawn at random"}
    ]);
    assert_eq!(
        deviation_problems("dhan_sandbox", Some(&devs)),
        Vec::<String>::new()
    );
    let found = deviation_problems("zerodha", Some(&devs));
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(found[0].contains("synthetic feed"), "{:?}", found);
}

/// The corpus runs exactly the app's brokers: all 38 of the catalog and
/// no folder beside them.
#[test]
fn the_corpus_covers_exactly_the_catalog_brokers() {
    let catalog: BTreeSet<&str> = ALL_BROKERS.iter().copied().collect();
    assert_eq!(catalog.len(), 38, "the catalog: {:?}", catalog);
    let modules: BTreeSet<&str> = BROKERS.iter().map(|(b, _)| *b).collect();
    assert_eq!(modules, catalog, "the corpus's broker modules");
    let on_disk = folders(&corpus_root());
    let disk: BTreeSet<&str> = on_disk.iter().map(String::as_str).collect();
    assert_eq!(disk, catalog, "the corpus's broker folders");
}

/// The modules still running without their broker are brokers of the
/// corpus, each listed once.
#[test]
fn the_without_broker_list_names_corpus_brokers() {
    let brokers: BTreeSet<&str> = BROKERS.iter().map(|(b, _)| *b).collect();
    let mut seen = BTreeSet::new();
    for b in WITHOUT_BROKER {
        assert!(
            brokers.contains(b),
            "WITHOUT_BROKER names {}, no corpus broker",
            b
        );
        assert!(seen.insert(b), "WITHOUT_BROKER lists {} twice", b);
    }
}
