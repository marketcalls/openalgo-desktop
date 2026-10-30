//! The web's tests that read the editor's sources and the import-format doc
//! (`test/test_flow_workflow_validator.py` registry and branch-handle
//! parity and its doc-example test, `test/test_flow_custom_legs.py` seeded
//! templates), over the desktop's carried-over copies. Ignored until the ui
//! lane's editor files and the carried `docs/prompt/flow-import-format.md`
//! are on the branch; the lead un-ignores them at restack. Until then the
//! doc's examples are pinned by the goldens, recorded from the web's copy.

use super::*;
use crate::flow::validator::{BRANCHING_NODE_TYPES, TRIGGER_NODE_TYPES, VALID_NODE_TYPES};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn sorted(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// `nodeTypes` keys in the editor's registry.
fn registry_types() -> BTreeSet<String> {
    let source = repo_file("src/components/flow/nodes/index.ts");
    let block = source
        .split("export const nodeTypes = {")
        .nth(1)
        .and_then(|rest| rest.split("} as const").next())
        .unwrap();
    let entry = regex::Regex::new(r"(?m)^\s*(\w+):\s*\w+Node,").unwrap();
    entry
        .captures_iter(block)
        .map(|c| c[1].to_string())
        .collect()
}

#[test]
fn the_validator_matches_the_editor_registry() {
    assert_eq!(registry_types(), sorted(&VALID_NODE_TYPES));
}

#[test]
fn every_node_rendering_branch_handles_is_a_branching_type() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/components/flow/nodes");
    let handle = regex::Regex::new(r#"id="(true|false|yes|no)""#).unwrap();
    let mut rendering = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix("Node.tsx") else {
            continue;
        };
        if stem == "Base" || !handle.is_match(&std::fs::read_to_string(&path).unwrap()) {
            continue;
        }
        let mut chars = stem.chars();
        let first = chars.next().unwrap().to_ascii_lowercase();
        let node_type = format!("{first}{}", chars.as_str());
        if VALID_NODE_TYPES.contains(&node_type.as_str()) {
            rendering.insert(node_type);
        }
    }
    let branching = sorted(&BRANCHING_NODE_TYPES);
    let missing: Vec<&String> = rendering.difference(&branching).collect();
    assert!(missing.is_empty(), "{missing:?}");
}

/// The (offset, optionType, action) triples the editor's template seeds for
/// a strategy (`seedLegsFromStrategy` in `src/lib/flow/customLegs.ts`).
fn seeded_legs(strategy: &str) -> Vec<(String, String, String)> {
    let source = repo_file("src/lib/flow/customLegs.ts");
    let body = source
        .split("export function seedLegsFromStrategy")
        .nth(1)
        .unwrap();
    let case = body
        .split(&format!("case '{strategy}':"))
        .nth(1)
        .and_then(|rest| rest.split("case '").next())
        .unwrap();
    let seeded = regex::Regex::new(r"leg\((width|'[^']+'), '([^']+)', (action|'[^']+')\)").unwrap();
    let resolve = |token: &str, name: &str, value: &str| {
        if token == name {
            value.to_string()
        } else {
            token.trim_matches('\'').to_string()
        }
    };
    seeded
        .captures_iter(case)
        .map(|c| {
            (
                resolve(&c[1], "width", "OTM2"),
                c[2].to_string(),
                resolve(&c[3], "action", "BUY"),
            )
        })
        .collect()
}

#[test]
fn every_seeded_template_is_a_basket_the_validator_accepts() {
    for strategy in ["iron_condor", "bull_call_spread", "bear_put_spread"] {
        let legs: Vec<Value> = seeded_legs(strategy)
            .into_iter()
            .map(|(offset, option_type, action)| {
                json!({"offset": offset, "optionType": option_type, "action": action, "quantity": 1,
                    "strikeMode": "OFFSET"})
            })
            .collect();
        assert!(!legs.is_empty(), "{strategy}");
        let data =
            json!({"strategy": "custom", "underlying": "NIFTY", "quantity": 1, "legs": legs});
        let options = ValidateOptions {
            require_name: false,
            strict: true,
        };
        assert_eq!(
            validate_workflow(&single_action("optionsMultiOrder", data), options),
            vec![]
        );
    }
}

/// The web's `_as_workflow_example`: a node snippet is wrapped with a start
/// trigger and an edge from it to every node.
fn as_workflow_example(value: &Value) -> Option<Value> {
    if value.get("nodes").is_some() && value.get("edges").is_some() {
        return Some(value.clone());
    }
    let snippets: Vec<Value> = match value {
        Value::Object(_) => vec![value.clone()],
        Value::Array(items) => items.clone(),
        _ => return None,
    };
    let is_node = |n: &Value| n["type"].is_string() && n["data"].is_object();
    if snippets.is_empty() || !snippets.iter().all(is_node) {
        return None;
    }
    let mut nodes: Vec<Value> = snippets
        .into_iter()
        .enumerate()
        .map(|(i, mut n)| {
            let at = json!({"x": 100, "y": (i + 1) * 100});
            let object = n.as_object_mut().unwrap();
            object
                .entry("id")
                .or_insert_with(|| json!(format!("documented_node_{}", i + 1)));
            object.entry("position").or_insert(at);
            n
        })
        .collect();
    let trigger = nodes
        .iter()
        .find(|n| TRIGGER_NODE_TYPES.contains(&n["type"].as_str().unwrap_or_default()))
        .map(|n| n["id"].clone());
    let trigger = trigger.unwrap_or_else(|| {
        nodes.insert(0, node("documented_trigger", "start", json!({})));
        json!("documented_trigger")
    });
    if nodes.len() == 1 {
        nodes.push(node(
            "documented_action",
            "log",
            json!({"message": "Prompt contract example"}),
        ));
    }
    let edges: Vec<Value> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n["id"] != trigger)
        .map(|(i, n)| json!({"id": format!("documented_edge_{}", i + 1), "source": trigger, "target": n["id"]}))
        .collect();
    Some(json!({"name": "Prompt contract example", "nodes": nodes, "edges": edges}))
}

#[test]
fn every_json_example_in_the_import_format_is_a_valid_workflow() {
    let prompt = repo_file("docs/prompt/flow-import-format.md");
    let fence = regex::Regex::new(r"(?ms)^```json\s*\n(.*?)^```\s*$").unwrap();
    let mut checked = 0;
    let mut failures = Vec::new();
    for (index, caps) in fence.captures_iter(&prompt).enumerate() {
        let values = serde_json::Deserializer::from_str(&caps[1]).into_iter::<Value>();
        for value in values {
            let value = value.unwrap_or_else(|e| panic!("fence {}: {e}", index + 1));
            let Some(payload) = as_workflow_example(&value) else {
                continue;
            };
            checked += 1;
            // A lone gate snippet (`"inputCount": 2`) is wrapped with one
            // edge from the trigger, so it can never be fully wired; the
            // desktop's V-085 check is about the doc's wrapper, not the doc.
            let wrapped_snippet = value.get("nodes").is_none();
            let errors: Vec<_> = strict(&payload)
                .into_iter()
                .filter(|e| !(wrapped_snippet && e.code == "unwired_gate_input"))
                .collect();
            if !errors.is_empty() {
                failures.push(format!("fence {}: {errors:?}", index + 1));
            }
        }
    }
    assert!(checked > 30, "only {checked} examples found");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
