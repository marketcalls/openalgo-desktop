//! The node registry: one line per node family in [`FAMILIES`], each family
//! one file here that builds its [`FlowNode`]s.
//!
//! Adding a family is one file plus one line. The walk looks a node up by
//! its type id; a type nobody registered is logged as unknown and passed
//! through, as the web's executor does. The three logic gates are not
//! nodes here: the walk evaluates them itself from their inputs' results.

pub mod conditions;
pub mod core;
pub mod logic;
pub mod math;
pub mod pyjson;
pub mod timing;

#[cfg(test)]
pub(crate) mod testkit;

use crate::flow::contract::node::FlowNode;
use std::collections::HashMap;
use std::sync::Arc;

/// One family of node types and the function that builds them.
#[derive(Clone, Copy)]
pub struct NodeFamily {
    /// Stable key, unique across [`FAMILIES`] (`core`, `orders`, ...).
    pub key: &'static str,
    pub build: fn() -> Vec<Arc<dyn FlowNode>>,
}

/// Every node family, one line each.
pub const FAMILIES: &[NodeFamily] = &[
    // start, group and the trigger nodes' run-time log lines.
    core::FAMILY,
    // log, variable, delay (the nodes-logic lane completes this family).
    logic::FAMILY,
];

/// The nodes by type id, built once.
pub struct NodeRegistry {
    by_type: HashMap<&'static str, Arc<dyn FlowNode>>,
}

impl NodeRegistry {
    /// Build every family's nodes. A type registered twice keeps the first
    /// and is logged; the registry test refuses it.
    pub fn new(families: &[NodeFamily]) -> Self {
        let mut by_type: HashMap<&'static str, Arc<dyn FlowNode>> = HashMap::new();
        for family in families {
            for node in (family.build)() {
                let t = node.node_type();
                if by_type.contains_key(t) {
                    tracing::error!("Flow node type {} is registered twice ({})", t, family.key);
                    continue;
                }
                by_type.insert(t, node);
            }
        }
        Self { by_type }
    }

    /// The node for a type id.
    pub fn get(&self, node_type: &str) -> Option<Arc<dyn FlowNode>> {
        self.by_type.get(node_type).cloned()
    }

    /// Every registered type id, sorted.
    pub fn types(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.by_type.keys().copied().collect();
        v.sort_unstable();
        v
    }
}

impl Default for NodeRegistry {
    fn default() -> Self {
        Self::new(FAMILIES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::contract::model::{is_node_type, GATE_TYPES};
    use std::collections::HashSet;

    /// Every family key and every node type is registered once, every type
    /// is one the editor knows, and the gates stay with the walk.
    #[test]
    fn every_node_type_is_registered_once() {
        let mut keys = HashSet::new();
        let mut types = HashSet::new();
        for family in FAMILIES {
            assert!(keys.insert(family.key), "family {} twice", family.key);
            for node in (family.build)() {
                let t = node.node_type();
                assert!(types.insert(t), "{} registered twice", t);
                assert!(is_node_type(t), "{} is not a node type the editor knows", t);
                assert!(!GATE_TYPES.contains(&t), "{} is evaluated by the walk", t);
            }
        }
        assert_eq!(NodeRegistry::default().types().len(), types.len());
    }
}
