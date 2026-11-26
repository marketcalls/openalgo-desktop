//! What the README promises about the Agent's indicators, against the
//! catalogue and the build the goldens were recorded on.

use std::path::Path;

use super::support::{json, REGISTRY};
use crate::agent::indicators::registry;

/// The README's Agent section, whitespace runs as one space so a phrase
/// matches across line wraps (and a Windows checkout's CRLF).
fn agent_section() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../README.md");
    let readme =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let start = readme
        .find("### AI Agent (`/agent`)")
        .expect("the README's Agent section");
    let section = &readme[start..];
    let end = section[1..]
        .find("\n### ")
        .map_or(section.len(), |at| at + 1);
    section[..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Review of 33c2f747: the README promised "the same results as OpenAlgo
/// web's `openalgo.ta`" while the numbers are those of the build the goldens
/// were recorded on (openalgo 2.0.5) and the web runs 2.4.1. It names that
/// build and each method carried from a later one, so moving `oa_core` and
/// re-recording the goldens fails here until the README says so.
#[test]
fn the_readme_names_the_build_whose_numbers_the_agent_gives() {
    let fixture = json(REGISTRY);
    let section = agent_section();

    let count = format!("Computes {} indicators", registry::count());
    assert!(section.contains(&count), "{section}");
    let build = fixture["openalgo"].as_str().expect("the recorded build");
    assert!(
        section.contains(&format!("the numbers of openalgo {build}")),
        "{section}"
    );
    let backported = fixture["backported"].as_object().expect("backported");
    for (method, version) in backported {
        let version = version.as_str().expect("a version");
        let carried = format!("{version}'s `{method}`");
        assert!(section.contains(&carried), "{carried}: {section}");
    }
    assert!(!section.contains("same results"), "{section}");
}
