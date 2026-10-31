//! Structural validation for Flow workflow payloads: the web's
//! `services/flow_workflow_validator.py`, rule for rule.
//!
//! The editor validates before it posts, but the routes are reachable
//! directly, so without a server-side check a malformed graph is stored and
//! only fails later, at activation or mid-execution against a live broker.
//! A workflow that cannot be rendered or executed never reaches the database.
//!
//! It works over [`serde_json::Value`], as the web works over the decoded
//! request dict, and returns the web's error list: the same `path`, `code`,
//! `message`, `expected` and `received`, in the same order. Callers mirror
//! the web's call sites (`blueprints/flow.py`, the executor, the Agent's
//! `flow_gen`):
//!
//! | Caller | Call |
//! | --- | --- |
//! | create and update (editor save) | `validate_workflow(&{name, nodes, edges}, ValidateOptions::DRAFT)` |
//! | import, replace, activate, Run Now, every trigger's run | `validate_workflow(&payload, ValidateOptions::COMPLETE)` |
//! | Agent `validate_flow` (a draft without a name) | `ValidateOptions { require_name: false, strict: true }` |
//! | import, replace, Agent | `migrate_legacy_node_data(nodes)` before validating |
//! | update, replace, Agent | `trigger_config(&nodes)` before and after, compared with `!=` |
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `catalog` | node types, required fields, condition and order vocabularies, limits |
//! | `structure` | the workflow, node and edge shape, ids, handles, gate slots, trigger count |
//! | `fields` | value rules for node fields, and the strict completeness rules |
//! | `legs` | the Margin basket and the custom options legs |
//! | `graph` | cycles, reachability from the trigger, gates wired to fewer inputs than declared |
//! | `migrate` | legacy node payloads, and the trigger configuration a change re-registers |
//! | `py` | Python's `str.strip()`, the template test and `==` over JSON values (`repr`, `str`, `float()` and `int()` are core's `flow::contract::pyfmt`) |
//!
//! Deviations from the web, each a web defect fixed rather than copied (the
//! ids are the parity checklist's, `.lane/105/parity.md`):
//!
//! - V-005: where the web iterates a Python `set` (the positive-number
//!   fields, the cycle search's start node) its order differs between
//!   processes. Here the fields go `quantity`, `splitSize`, `lots` (the
//!   source order, and the order the web uses most often: 86 of 400 hash
//!   seeds), and the cycle search starts from the nodes in the order listed.
//! - V-006: input the web's validator raises on, answering a 500 (an array
//!   or object as an edge endpoint, a node type or a gate id), is reported by
//!   the structural checks and passed over by the handle checks.
//! - V-053: an httpRequest timeout of `"NaN"` is refused, as its own message
//!   says it must be; the web's range comparisons let NaN through. (A custom
//!   leg's strike of `"NaN"` or `"Infinity"` text passes as on the web, lead
//!   decision 36: the executor refuses it when it resolves the strike.)
//! - V-059: an optionsMultiOrder node's strategy is read as the executor
//!   reads it (`strategy`, else `strategyType`, else `custom`), not defaulted
//!   to `straddle`, so a node with no strategy and no legs is refused.
//! - V-085: a gate wired to fewer inputs than the executor counts for it
//!   (`executor::walk::declared_inputs`: an AND or OR gate with no readable
//!   `inputCount` has two) is refused at the strict level with the
//!   executor's sentence; the web accepts it and the run then fails every
//!   time (lead decisions 8 and 25).
//! - V-113: a legacy positionCheck `>=` or `<=` keeps its inclusive meaning:
//!   the node's conditions are strict and read whole-number quantities, so
//!   the threshold moves by one (`>= 5` becomes `quantity_above` 4). The web
//!   maps it onto the strict condition unchanged (lead decision 23).
//! - A gate's `expected` slot list stops at [`MAX_EDGES`] entries (a gate can
//!   never receive more); the web builds the whole list for any
//!   `inputCount`, however large. A recorded denial-of-service bound (lead
//!   decision 36, V-084).
//! - A JSON object quoted in a message lists its keys sorted (`pyfmt`).
//!
//! Digits in a gate slot (`input-N`) and in an `inputCount` are read as
//! Python's `\d` and `int()` read them, Unicode decimal digits included
//! (`py::ascii_digits`, lead decision 36).
//!
//! JSON written as text (httpRequest `headers`, Margin `positionsJson`) is
//! read with `flow::nodes::pyjson`, Python's `json.loads`, so `NaN` and
//! `Infinity` parse as on the web (lead decision 24); such a number is kept
//! as its Python text (`'nan'`), so a sentence that quotes it shows quotes
//! the web's does not.

mod catalog;
mod fields;
mod graph;
mod legs;
mod migrate;
pub(crate) mod py;
mod structure;

#[cfg(test)]
mod tests;

use serde::Serialize;
use serde_json::Value;

pub use catalog::{
    BRANCHING_NODE_TYPES, DECORATIVE_NODE_TYPES, EITHER_REQUIRED_FIELDS, HTTP_METHODS,
    HTTP_TIMEOUT_MAX_MS, HTTP_TIMEOUT_MIN_MS, MAX_EDGES, MAX_NODES, REQUIRED_NODE_FIELDS,
    TRIGGER_NODE_TYPES, VALID_NODE_TYPES,
};
pub use migrate::{migrate_legacy_node_data, trigger_config, TriggerConfig, TRIGGER_CONFIG_FIELDS};

/// The two levels the web validates at (its `require_name` and `strict`
/// keyword arguments).
///
/// Structure (shape, ids, known node types, edge endpoints, branch handles,
/// and every supplied value) is always checked: a graph failing it is
/// corrupt however incomplete the user's work is. Completeness (required
/// fields, exactly one trigger, reachability, no cycles) is `strict` only:
/// enforced at import, activation and execution, where the workflow is
/// presented as finished, and skipped on save so a half-built graph stays
/// savable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidateOptions {
    /// The workflow must carry a non-empty `name`.
    pub require_name: bool,
    /// Completeness is checked as well as structure.
    pub strict: bool,
}

impl ValidateOptions {
    /// Import, replace, activation, execution: the web's defaults.
    pub const COMPLETE: ValidateOptions = ValidateOptions {
        require_name: true,
        strict: true,
    };
    /// The editor's create and update saves.
    pub const DRAFT: ValidateOptions = ValidateOptions {
        require_name: false,
        strict: false,
    };
}

impl Default for ValidateOptions {
    fn default() -> Self {
        Self::COMPLETE
    }
}

/// One problem, with where it is and why: the web's error dict. Serializes
/// with the web's keys in the web's order, `expected` and `received` only
/// when the web includes them (it leaves out a `None`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidationError {
    /// A JSON pointer into the payload, such as `/nodes/1/data/quantity`.
    pub path: String,
    /// A stable machine code, such as `missing_required_field`.
    pub code: &'static str,
    /// The sentence shown to the trader.
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received: Option<Value>,
}

impl ValidationError {
    pub(crate) fn new(
        path: impl Into<String>,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            code,
            message: message.into(),
            expected: None,
            received: None,
        }
    }

    /// What a valid value would have been.
    pub(crate) fn expected(mut self, expected: impl Into<Value>) -> Self {
        self.expected = Some(expected.into());
        self
    }

    /// The value found, left out when it is `None`, as the web does.
    pub(crate) fn received(mut self, received: &Value) -> Self {
        self.received = (!received.is_null()).then(|| received.clone());
        self
    }
}

/// Every problem with `payload`; an empty list means the workflow passes.
pub fn validate_workflow(payload: &Value, options: ValidateOptions) -> Vec<ValidationError> {
    structure::validate(payload, options)
}
