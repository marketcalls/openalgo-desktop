//! Cycles and nodes the trigger can never reach (the web's `_graph_errors`),
//! then gates wired to fewer inputs than they declare. Run only at the
//! strict level and only on a graph whose ids and edge endpoints are already
//! sound.

use super::catalog::{is_one_of, DECORATIVE_NODE_TYPES, GATE_NODE_TYPES};
use super::structure::{get, NodeIds, NULL};
use super::{py, ValidationError};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Colour {
    White,
    Grey,
    Black,
}

pub(super) fn graph_errors(
    nodes: &[Value],
    edges: &[Value],
    ids: &NodeIds,
) -> Vec<ValidationError> {
    let mut adjacency: HashMap<&str, Vec<&str>> = ids
        .order
        .iter()
        .map(|id| (id.as_str(), Vec::new()))
        .collect();
    for edge in edges.iter().filter_map(Value::as_object) {
        let (Some(source), Some(target)) =
            (get(edge, "source").as_str(), get(edge, "target").as_str())
        else {
            continue;
        };
        if ids.set.contains(target) {
            if let Some(next) = adjacency.get_mut(source) {
                next.push(target);
            }
        }
    }

    let mut errors = Vec::new();
    // The executor caps depth and visits rather than detecting a loop, so a
    // cycle burns the whole budget and truncates the run instead of
    // reporting anything.
    if let Some(cycle) = first_cycle(&ids.order, &adjacency) {
        let shown: Vec<&str> = cycle.iter().take(6).copied().collect();
        errors.push(
            ValidationError::new(
                "/edges",
                "cycle",
                format!(
                    "The graph contains a cycle, which runs until the executor's visit limit and \
                     then truncates rather than reporting a loop: {}",
                    shown.join(" -> ")
                ),
            )
            .expected("an acyclic graph")
            .received(&Value::from(shown)),
        );
    }

    // A node the trigger cannot reach never runs, and nothing at run time
    // says so.
    if !ids.triggers.is_empty() {
        let seen = reachable(&ids.triggers, &adjacency);
        // `group` is a visual container with no execution behaviour, so it is
        // legitimately unconnected while the nodes drawn inside it run.
        let decorative: HashSet<&str> = nodes
            .iter()
            .filter_map(Value::as_object)
            .filter(|n| {
                get(n, "type")
                    .as_str()
                    .is_some_and(|t| is_one_of(&DECORATIVE_NODE_TYPES, t))
            })
            .filter_map(|n| get(n, "id").as_str())
            .collect();
        let mut unreachable: Vec<&str> = ids
            .order
            .iter()
            .map(String::as_str)
            .filter(|id| !seen.contains(id) && !decorative.contains(id))
            .collect();
        unreachable.sort_unstable();
        if !unreachable.is_empty() {
            let shown: Vec<&str> = unreachable.into_iter().take(8).collect();
            errors.push(
                ValidationError::new(
                    "/nodes",
                    "unreachable",
                    format!(
                        "These nodes cannot be reached from the trigger and will never run: {}",
                        shown.join(", ")
                    ),
                )
                .expected("every node reachable from the trigger")
                .received(&Value::from(shown)),
            );
        }
    }
    errors.extend(unwired_gate_errors(nodes, edges));
    errors
}

/// A gate configured for more inputs than are wired (parity V-085, a check
/// the web's validator lacks). The executor refuses such a gate on every
/// run rather than evaluating part of the condition, so a workflow holding
/// one could never complete; it is refused here, at import and activation,
/// with the executor's own sentence. Counted as the executor counts: an
/// integer `inputCount` (a JSON `true` reads as 1, as Python's `int` does)
/// against every edge that targets the gate.
fn unwired_gate_errors(nodes: &[Value], edges: &[Value]) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        let Some(node) = node.as_object() else {
            continue;
        };
        let node_type = get(node, "type").as_str().unwrap_or_default();
        if !is_one_of(&GATE_NODE_TYPES, node_type) {
            continue;
        }
        let declared = get(node, "data").get("inputCount").unwrap_or(&NULL);
        let count = match declared {
            Value::Bool(b) => Some(i64::from(*b)),
            Value::Number(n) if !n.is_f64() => Some(n.as_i64().unwrap_or(i64::MAX)),
            _ => None,
        };
        let id = get(node, "id");
        let wired = edges
            .iter()
            .filter_map(Value::as_object)
            .filter(|e| get(e, "target") == id)
            .count();
        if !count.is_some_and(|c| c > i64::try_from(wired).unwrap_or(i64::MAX)) {
            continue;
        }
        errors.push(
            ValidationError::new(
                format!("/nodes/{i}/data/inputCount"),
                "unwired_gate_input",
                format!(
                    "{node_type} is configured for {} inputs but only {wired} are wired. Wire the \
                     rest, or lower the input count, rather than evaluating on part of the \
                     condition.",
                    py::str_of(declared)
                ),
            )
            .expected(declared.clone())
            .received(&Value::from(wired)),
        );
    }
    errors
}

/// The first cycle a depth-first search meets, starting from each node in
/// turn, as the path from the node that closes it back to that node: the
/// web's recursive `visit`, unrolled so a 500-node chain cannot exhaust the
/// stack.
fn first_cycle<'a>(
    order: &'a [String],
    adjacency: &HashMap<&'a str, Vec<&'a str>>,
) -> Option<Vec<&'a str>> {
    let mut colour: HashMap<&str, Colour> = order
        .iter()
        .map(|id| (id.as_str(), Colour::White))
        .collect();
    for start in order.iter().map(String::as_str) {
        if colour.get(start) != Some(&Colour::White) {
            continue;
        }
        colour.insert(start, Colour::Grey);
        let mut path = vec![start];
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        while let Some((node, next_index)) = stack.last_mut() {
            let children = adjacency.get(*node).map_or(&[][..], Vec::as_slice);
            let Some(next) = children.get(*next_index).copied() else {
                colour.insert(*node, Colour::Black);
                stack.pop();
                path.pop();
                continue;
            };
            *next_index += 1;
            match colour.get(next) {
                Some(Colour::Grey) => {
                    let from = path.iter().position(|n| *n == next).unwrap_or(0);
                    let mut cycle = path[from..].to_vec();
                    cycle.push(next);
                    return Some(cycle);
                }
                Some(Colour::White) => {
                    colour.insert(next, Colour::Grey);
                    path.push(next);
                    stack.push((next, 0));
                }
                _ => {}
            }
        }
    }
    None
}

/// Every node a walk from the triggers reaches.
fn reachable<'a>(
    triggers: &'a [String],
    adjacency: &HashMap<&'a str, Vec<&'a str>>,
) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut stack: Vec<&str> = triggers.iter().map(String::as_str).collect();
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        if let Some(next) = adjacency.get(node) {
            stack.extend(next.iter().copied());
        }
    }
    seen
}
