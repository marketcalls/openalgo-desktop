//! The corpus folder and the case lists agree: a case folder no test runs,
//! or a listed case with no folder, fails here; and every case reads and
//! checks (its `case.json`, script and frame files) without a socket.

use super::harness::{corpus_root, Case};
use super::BROKERS;
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
