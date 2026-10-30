//! Tests of the Flow validator against what OpenAlgo web answers.
//!
//! | Module | What it pins |
//! | --- | --- |
//! | `web_validator` | `test/test_flow_workflow_validator.py`, ported |
//! | `web_regressions` | the validator parts of the web's other Flow tests |
//! | `goldens` | every error, field for field, as the web's own validator returned it |
//! | `rules` | rules the web's tests leave unpinned, and the desktop's fixes |
//! | `py_values` | Python's `repr`, `float()`, `int()` and `==` over JSON values |
//! | `editor_parity` | the web's tests over the editor's sources and the import-format doc (ignored until carried) |

mod editor_parity;
mod editor_parity_docs;
mod goldens;
mod py_values;
mod rules;
mod web_regressions;
mod web_validator;

use super::{validate_workflow, ValidateOptions, ValidationError};
use serde_json::{json, Value};

pub(super) fn wf(nodes: Vec<Value>, edges: Vec<Value>) -> Value {
    json!({"name": "W", "nodes": nodes, "edges": edges})
}

pub(super) fn node(id: &str, node_type: &str, data: Value) -> Value {
    json!({"id": id, "type": node_type, "position": {"x": 0, "y": 0}, "data": data})
}

pub(super) fn edge(id: &str, source: &str, target: &str) -> Value {
    json!({"id": id, "source": source, "target": target})
}

/// A trigger wired to one action node: the shape most web tests build.
pub(super) fn single_action(node_type: &str, data: Value) -> Value {
    wf(
        vec![
            node("start", "start", json!({})),
            node("action", node_type, data),
        ],
        vec![edge("edge", "start", "action")],
    )
}

pub(super) fn strict(payload: &Value) -> Vec<ValidationError> {
    validate_workflow(payload, ValidateOptions::COMPLETE)
}

pub(super) fn draft(payload: &Value) -> Vec<ValidationError> {
    validate_workflow(payload, ValidateOptions::DRAFT)
}

/// The web's `_strict_node_errors`: the node alone, as the trigger slot.
pub(super) fn strict_node(node_type: &str, data: Value) -> Vec<ValidationError> {
    strict(&wf(vec![node("trigger", node_type, data)], vec![]))
}

/// The web's `_permissive_node_errors`: the same node at the save level.
pub(super) fn draft_node(node_type: &str, data: Value) -> Vec<ValidationError> {
    draft(&wf(vec![node("trigger", node_type, data)], vec![]))
}

pub(super) fn codes(errors: &[ValidationError]) -> Vec<&'static str> {
    errors.iter().map(|e| e.code).collect()
}

/// Whether an error with this code sits at a path ending in `suffix`.
pub(super) fn has(errors: &[ValidationError], code: &str, suffix: &str) -> bool {
    errors
        .iter()
        .any(|e| e.code == code && e.path.ends_with(suffix))
}

/// Whether any error sits at a path ending in `suffix`.
pub(super) fn has_path(errors: &[ValidationError], suffix: &str) -> bool {
    errors.iter().any(|e| e.path.ends_with(suffix))
}

/// `dict(base, **extra)`.
pub(super) fn with(base: &Value, extra: Value) -> Value {
    let mut merged = base.clone();
    if let (Some(target), Some(source)) = (merged.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    merged
}
