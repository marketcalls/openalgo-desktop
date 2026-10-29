//! The body of the web's `validate_workflow`: the workflow, node and edge
//! shape, ids, known node types, edge endpoints, branch handles, gate input
//! slots and the one-trigger rule, in the order the web reports them.

use super::catalog::{
    is_one_of, BRANCHING_HANDLES, BRANCHING_NODE_TYPES, MAX_EDGES, MAX_NODES, TRIGGER_NODE_TYPES,
    VALID_NODE_TYPES,
};
use super::{fields, graph, py, ValidateOptions, ValidationError};
use crate::agent::tools::values::Pattern;
use serde_json::{Map, Value};
use std::collections::HashSet;

/// The value a missing key reads as.
pub(super) static NULL: Value = Value::Null;

/// `input-N`, the numbered input slot a gate renders.
static GATE_SLOT: Pattern = Pattern::new(r"^input-([0-9]+)$");

/// `mapping.get(key)`: a missing key reads as `None`.
pub(super) fn get<'a>(map: &'a Map<String, Value>, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&NULL)
}

/// The node ids the graph checks run over, and the triggers found.
pub(super) struct NodeIds {
    /// Valid, unique ids, in the order the nodes are listed.
    pub(super) order: Vec<String>,
    pub(super) set: HashSet<String>,
    /// `str(id)` of every trigger node, in order.
    pub(super) triggers: Vec<String>,
}

pub(super) fn validate(payload: &Value, options: ValidateOptions) -> Vec<ValidationError> {
    let Some(workflow) = payload.as_object() else {
        return vec![
            ValidationError::new("/", "invalid_type", "Workflow must be a JSON object")
                .expected("object")
                .received(&Value::from(py::type_name(payload))),
        ];
    };
    let mut errors = Vec::new();

    let name = get(workflow, "name");
    if options.require_name && !name.as_str().is_some_and(|n| !py::is_blank(n)) {
        errors.push(
            ValidationError::new("/name", "required", "Workflow needs a non-empty name")
                .expected("string")
                .received(name),
        );
    }
    let nodes = list_field(workflow, "nodes", &mut errors);
    let edges = list_field(workflow, "edges", &mut errors);
    for (path, items, limit, noun) in [
        ("/nodes", nodes, MAX_NODES, "nodes"),
        ("/edges", edges, MAX_EDGES, "edges"),
    ] {
        if items.len() > limit {
            errors.push(
                ValidationError::new(
                    path,
                    "too_large",
                    format!("Workflow has more than {limit} {noun}"),
                )
                .expected(limit)
                .received(&Value::from(items.len())),
            );
        }
    }

    let ids = check_nodes(nodes, options.strict, &mut errors);
    check_edges(edges, &ids, &mut errors);
    check_source_handles(nodes, edges, &mut errors);
    check_gate_slots(nodes, edges, &mut errors);
    check_trigger_count(nodes, &ids.triggers, options.strict, &mut errors);

    // Only meaningful once ids and endpoints are sound.
    if options.strict && errors.is_empty() {
        errors.extend(graph::graph_errors(nodes, edges, &ids));
    }
    errors
}

/// The `nodes` or `edges` array, or an empty one after reporting it.
fn list_field<'a>(
    workflow: &'a Map<String, Value>,
    key: &str,
    errors: &mut Vec<ValidationError>,
) -> &'a [Value] {
    let value = get(workflow, key);
    if let Some(items) = value.as_array() {
        return items;
    }
    let article = if key == "edges" { "an" } else { "a" };
    errors.push(
        ValidationError::new(
            format!("/{key}"),
            "required",
            format!("Workflow needs {article} {key} array"),
        )
        .expected("array")
        .received(&type_name_unless_null(value)),
    );
    &[]
}

/// `type(value).__name__`, or `None` (left out of the error) for a null.
fn type_name_unless_null(value: &Value) -> Value {
    if value.is_null() {
        Value::Null
    } else {
        Value::from(py::type_name(value))
    }
}

fn check_nodes(nodes: &[Value], strict: bool, errors: &mut Vec<ValidationError>) -> NodeIds {
    let mut ids = NodeIds {
        order: Vec::new(),
        set: HashSet::new(),
        triggers: Vec::new(),
    };
    for (i, node) in nodes.iter().enumerate() {
        let base = format!("/nodes/{i}");
        let Some(node) = node.as_object() else {
            errors.push(
                ValidationError::new(base, "invalid_type", "Node must be an object")
                    .expected("object")
                    .received(&Value::from(py::type_name(node))),
            );
            continue;
        };
        check_node_id(&base, get(node, "id"), &mut ids, errors);
        check_node_type(&base, node, &mut ids, errors);

        let data = get(node, "data");
        // Value-level checks run at every level, not just `strict`: a field
        // holding an invalid constant is wrong however incomplete the graph
        // is, and the editor saves through the non-strict path.
        if let (Some(fields), Some(node_type)) = (data.as_object(), get(node, "type").as_str()) {
            errors.extend(fields::value_errors(&base, node_type, fields, strict));
            if strict {
                errors.extend(fields::completeness_errors(&base, node_type, fields));
            }
        }
        if !data.is_object() {
            let received = type_name_unless_null(data);
            errors.push(
                ValidationError::new(
                    format!("{base}/data"),
                    "required",
                    "Node needs a data object",
                )
                .expected("object")
                .received(&received),
            );
        }
        check_position(&base, get(node, "position"), errors);
    }
    ids
}

fn check_node_id(base: &str, id: &Value, ids: &mut NodeIds, errors: &mut Vec<ValidationError>) {
    match id.as_str() {
        Some(text) if !py::is_blank(text) => {
            if ids.set.contains(text) {
                errors.push(
                    ValidationError::new(
                        format!("{base}/id"),
                        "duplicate",
                        format!("Duplicate node id '{text}'"),
                    )
                    .expected("unique id")
                    .received(id),
                );
            } else {
                ids.set.insert(text.to_string());
                ids.order.push(text.to_string());
            }
        }
        _ => errors.push(
            ValidationError::new(
                format!("{base}/id"),
                "required",
                "Node needs a non-empty string id",
            )
            .expected("string")
            .received(id),
        ),
    }
}

fn check_node_type(
    base: &str,
    node: &Map<String, Value>,
    ids: &mut NodeIds,
    errors: &mut Vec<ValidationError>,
) {
    let node_type = get(node, "type");
    match node_type.as_str() {
        Some(t) if !t.is_empty() && !is_one_of(&VALID_NODE_TYPES, t) => errors.push(
            ValidationError::new(
                format!("{base}/type"),
                "unknown_node_type",
                format!(
                    "'{t}' is not a Flow node type. Node types are fixed; inventing one makes the \
                     workflow unrenderable."
                ),
            )
            .expected("one of the documented node types")
            .received(node_type),
        ),
        Some(t) if !t.is_empty() => {
            if is_one_of(&TRIGGER_NODE_TYPES, t) {
                ids.triggers.push(py::str_of(get(node, "id")));
            }
        }
        _ => errors.push(
            ValidationError::new(format!("{base}/type"), "required", "Node needs a type")
                .expected("string")
                .received(node_type),
        ),
    }
}

fn check_position(base: &str, position: &Value, errors: &mut Vec<ValidationError>) {
    let Some(position) = position.as_object() else {
        let received = type_name_unless_null(position);
        errors.push(
            ValidationError::new(
                format!("{base}/position"),
                "required",
                "Node needs a position {x, y}",
            )
            .expected("object")
            .received(&received),
        );
        return;
    };
    for axis in ["x", "y"] {
        let value = get(position, axis);
        if !value.is_number() {
            errors.push(
                ValidationError::new(
                    format!("{base}/position/{axis}"),
                    "invalid_type",
                    format!("position.{axis} must be a number"),
                )
                .expected("number")
                .received(value),
            );
        }
    }
}

fn check_edges(edges: &[Value], ids: &NodeIds, errors: &mut Vec<ValidationError>) {
    let mut edge_ids: HashSet<&str> = HashSet::new();
    for (i, edge) in edges.iter().enumerate() {
        let base = format!("/edges/{i}");
        let Some(edge) = edge.as_object() else {
            errors.push(
                ValidationError::new(base, "invalid_type", "Edge must be an object")
                    .expected("object")
                    .received(&Value::from(py::type_name(edge))),
            );
            continue;
        };
        let id = get(edge, "id");
        match id.as_str() {
            Some(text) if !py::is_blank(text) => {
                if !edge_ids.insert(text) {
                    errors.push(
                        ValidationError::new(
                            format!("{base}/id"),
                            "duplicate",
                            format!("Duplicate edge id '{text}'"),
                        )
                        .expected("unique id")
                        .received(id),
                    );
                }
            }
            _ => errors.push(
                ValidationError::new(
                    format!("{base}/id"),
                    "required",
                    "Edge needs a non-empty string id",
                )
                .expected("string")
                .received(id),
            ),
        }
        for endpoint in ["source", "target"] {
            let value = get(edge, endpoint);
            match value.as_str() {
                Some(node_id) if !node_id.is_empty() => {
                    // A dangling edge renders as a broken connection and
                    // silently drops whatever branch it was meant to carry.
                    if !ids.set.is_empty() && !ids.set.contains(node_id) {
                        errors.push(
                            ValidationError::new(
                                format!("{base}/{endpoint}"),
                                "dangling_edge",
                                format!(
                                    "Edge {endpoint} '{node_id}' is not a node in this workflow"
                                ),
                            )
                            .expected("an existing node id")
                            .received(value),
                        );
                    }
                }
                _ => errors.push(
                    ValidationError::new(
                        format!("{base}/{endpoint}"),
                        "required",
                        format!("Edge needs a {endpoint} node id"),
                    )
                    .expected("string")
                    .received(value),
                ),
            }
        }
    }
}

/// `{node.id: node.type}` over nodes whose id is a string; a repeated id
/// keeps the last node's type, as a Python dict comprehension does.
fn type_of<'a>(nodes: &'a [Value], id: &Value) -> &'a Value {
    let Some(id) = id.as_str() else {
        return &NULL;
    };
    nodes
        .iter()
        .rev()
        .filter_map(Value::as_object)
        .find(|n| get(n, "id").as_str() == Some(id))
        .map_or(&NULL, |n| get(n, "type"))
}

/// Source handles each node kind may emit. Condition nodes and gates fork;
/// anything else on an edge is a typo that silently drops the branch.
fn check_source_handles(nodes: &[Value], edges: &[Value], errors: &mut Vec<ValidationError>) {
    for (i, edge) in edges.iter().enumerate() {
        let Some(edge) = edge.as_object() else {
            continue;
        };
        let handle = get(edge, "sourceHandle");
        if handle.is_null() || handle.as_str() == Some("") {
            continue; // an unconditional edge is legitimate
        }
        let source_type = type_of(nodes, get(edge, "source"));
        let branching = source_type
            .as_str()
            .is_some_and(|t| is_one_of(&BRANCHING_NODE_TYPES, t));
        let shown = py::str_of(handle);
        let path = format!("/edges/{i}/sourceHandle");
        if branching && !BRANCHING_HANDLES.contains(&shown.as_str()) {
            errors.push(
                ValidationError::new(
                    path,
                    "invalid_source_handle",
                    format!(
                        "'{shown}' is not a branch of a {} node, so this edge is never followed and \
                         its branch is silently dropped.",
                        py::str_of(source_type)
                    ),
                )
                .expected(BRANCHING_HANDLES.to_vec())
                .received(handle),
            );
        } else if !branching && py::truthy(source_type) {
            // Non-branching nodes render a single unnamed source handle, so
            // any named one points at a socket that does not exist.
            errors.push(
                ValidationError::new(
                    path,
                    "invalid_source_handle",
                    format!(
                        "A {} node has no '{shown}' output handle; it emits a single unconditional \
                         output.",
                        py::str_of(source_type)
                    ),
                )
                .expected("no sourceHandle")
                .received(handle),
            );
        }
    }
}

/// A gate's input count, tolerant of a malformed value (the web's
/// `_slot_count`): two when unset or unreadable, never fewer than one.
fn slot_count(value: &Value) -> i128 {
    if value.is_null() || value.as_str() == Some("") {
        return 2;
    }
    py::int_of(value).map_or(2, |n| n.max(1))
}

/// The input count of the gate `target` names, if it names an AND or OR
/// gate. Keyed like the web's dict: any hashable id, the last node wins.
fn gate_slots(nodes: &[Value], target: &Value) -> Option<i128> {
    nodes
        .iter()
        .rev()
        .filter_map(Value::as_object)
        .find_map(|n| {
            let gate = matches!(get(n, "type").as_str(), Some("andGate" | "orGate"));
            let data = get(n, "data").as_object()?;
            (gate && py::same_key(get(n, "id"), target))
                .then(|| slot_count(get(data, "inputCount")))
        })
}

/// `targetHandle: "input-N"` must name a slot the gate has: a higher N is
/// never filled, so the gate waits for an input that never arrives.
fn check_gate_slots(nodes: &[Value], edges: &[Value], errors: &mut Vec<ValidationError>) {
    for (i, edge) in edges.iter().enumerate() {
        let Some(edge) = edge.as_object() else {
            continue;
        };
        let handle = get(edge, "targetHandle");
        if handle.is_null() || handle.as_str() == Some("") {
            continue;
        }
        let shown = py::str_of(handle);
        let slot = GATE_SLOT
            .captures(&shown)
            .and_then(|caps| caps.get(1))
            .map(|n| py::int_from_str(n.as_str()).unwrap_or(i128::MAX));
        let path = format!("/edges/{i}/targetHandle");
        let Some(slots) = gate_slots(nodes, get(edge, "target")) else {
            // Only gates render numbered input slots.
            if slot.is_some() {
                let target_type = type_of(nodes, get(edge, "target"));
                let kind = if py::truthy(target_type) {
                    py::str_of(target_type)
                } else {
                    "non-gate".to_string()
                };
                errors.push(
                    ValidationError::new(
                        path,
                        "invalid_target_handle",
                        format!("Only gate nodes have numbered input slots; a {kind} node has no '{shown}'."),
                    )
                    .expected("no targetHandle")
                    .received(handle),
                );
            }
            continue;
        };
        if slot.is_none_or(|n| n >= slots) {
            let listed = usize::try_from(slots).unwrap_or(MAX_EDGES).min(MAX_EDGES);
            let expected: Vec<String> = (0..listed).map(|n| format!("input-{n}")).collect();
            errors.push(
                ValidationError::new(
                    path,
                    "invalid_target_handle",
                    format!(
                        "'{shown}' is not an input slot on this gate, which has {slots}. The gate \
                         would wait for an input that never arrives."
                    ),
                )
                .expected(expected)
                .received(handle),
            );
        }
    }
}

fn check_trigger_count(
    nodes: &[Value],
    triggers: &[String],
    strict: bool,
    errors: &mut Vec<ValidationError>,
) {
    if !strict {
        return;
    }
    if nodes.is_empty() {
        errors.push(
            ValidationError::new(
                "/nodes",
                "no_trigger",
                "Workflow has no nodes, so it can never run.",
            )
            .expected("at least a trigger node")
            .received(&Value::from(0)),
        );
    } else if triggers.is_empty() {
        errors.push(
            ValidationError::new(
                "/nodes",
                "no_trigger",
                format!(
                    "Workflow has no trigger node, so it can never run. Add one of: {}",
                    TRIGGER_NODE_TYPES.join(", ")
                ),
            )
            .expected("exactly one trigger")
            .received(&Value::from(0)),
        );
    } else if triggers.len() > 1 {
        // The executor walks from the first trigger it finds; the rest of the
        // graph never executes and nothing reports why.
        errors.push(
            ValidationError::new(
                "/nodes",
                "multiple_triggers",
                "Workflow has more than one trigger. Only the first would run and everything \
                 downstream of the others would be silently skipped.",
            )
            .expected("exactly one trigger")
            .received(&Value::from(triggers.to_vec())),
        );
    }
}
