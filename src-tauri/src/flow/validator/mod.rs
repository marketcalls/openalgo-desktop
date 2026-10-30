//! The workflow validator (the web's `services/flow_workflow_validator.py`),
//! with the web's signatures: [`validate_workflow`], [`migrate_legacy_node_data`]
//! and [`trigger_config`].
//!
//! This file holds the interface (the validator lane's published API) and
//! the top-level shape checks the core routes need; the validator lane's
//! file replaces it at restack, with the web's rules, rule for rule and
//! message for message, under these signatures.

use crate::flow::contract::model::TRIGGER_TYPES;
use crate::flow::contract::pyfmt;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// One problem, as the web's `_err` builds it: `{path, code, message}` plus
/// `expected` and `received` when known.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidationError {
    pub path: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received: Option<Value>,
}

impl ValidationError {
    pub fn new(
        path: &str,
        code: &str,
        message: &str,
        expected: Option<Value>,
        received: Option<Value>,
    ) -> Self {
        Self {
            path: path.into(),
            code: code.into(),
            message: message.into(),
            expected,
            received,
        }
    }
}

/// Which checks run (the web's keyword arguments).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidateOptions {
    /// The workflow needs a non-empty name.
    pub require_name: bool,
    /// Completeness as well as structure: required node fields, exactly
    /// one trigger, reachability, cycles (import, replace, activation, Run
    /// Now and every run).
    pub strict: bool,
}

impl ValidateOptions {
    /// The web's defaults.
    pub const COMPLETE: ValidateOptions = ValidateOptions {
        require_name: true,
        strict: true,
    };
    /// A save from the editor (create, PUT): structure only, name optional.
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

/// Every problem with `payload`; empty means it passes.
pub fn validate_workflow(payload: &Value, checks: ValidateOptions) -> Vec<ValidationError> {
    let Value::Object(map) = payload else {
        return vec![ValidationError::new(
            "/",
            "invalid_type",
            "Workflow must be a JSON object",
            Some(json!("object")),
            Some(json!(pyfmt::type_name(payload))),
        )];
    };
    let mut errors = Vec::new();
    let name = map.get("name");
    let named = matches!(name, Some(Value::String(s)) if !s.trim().is_empty());
    if checks.require_name && !named {
        errors.push(ValidationError::new(
            "/name",
            "required",
            "Workflow needs a non-empty name",
            Some(json!("string")),
            name.filter(|v| !v.is_null()).cloned(),
        ));
    }
    for (key, message) in [
        ("nodes", "Workflow needs a nodes array"),
        ("edges", "Workflow needs an edges array"),
    ] {
        match map.get(key) {
            Some(Value::Array(_)) => {}
            other => errors.push(ValidationError::new(
                &format!("/{}", key),
                "required",
                message,
                Some(json!("array")),
                other
                    .filter(|v| !v.is_null())
                    .map(|v| json!(pyfmt::type_name(v))),
            )),
        }
    }
    errors
}

/// Upgrade legacy node payloads before validation; returns the nodes and a
/// note for each change.
pub fn migrate_legacy_node_data(nodes: Vec<Value>) -> (Vec<Value>, Vec<String>) {
    (nodes, Vec::new())
}

/// The trigger fields registered at activation rather than read per run
/// (`TRIGGER_CONFIG_FIELDS`); a change in any needs the trigger
/// re-registered.
pub const TRIGGER_CONFIG_FIELDS: &[&str] = &[
    "scheduleType",
    "time",
    "days",
    "executeAt",
    "intervalMinutes",
    "intervalValue",
    "intervalUnit",
    "marketHoursOnly",
    "symbol",
    "exchange",
    "condition",
    "price",
    "priceLower",
    "priceUpper",
    "percentage",
    "expiration",
    "orderId",
    "status",
    "trigger",
];

/// The registered configuration of a graph's trigger nodes, by trigger
/// type; compared with `!=` as the web compares its dicts.
pub type TriggerConfig = Value;

/// The activation-relevant configuration of a graph's trigger node(s);
/// two graphs with equal config can be swapped under a running workflow
/// (`trigger_config`).
pub fn trigger_config(nodes: &[Value]) -> TriggerConfig {
    let mut config = Map::new();
    for node in nodes {
        let Some(t) = node.get("type").and_then(Value::as_str) else {
            continue;
        };
        if !TRIGGER_TYPES.contains(&t) {
            continue;
        }
        let data = match node.get("data") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => continue,
        };
        let fields: Map<String, Value> = TRIGGER_CONFIG_FIELDS
            .iter()
            .filter_map(|f| data.get(*f).map(|v| (f.to_string(), v.clone())))
            .collect();
        config.insert(t.to_string(), Value::Object(fields));
    }
    Value::Object(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_top_level_shape_is_checked_as_the_web_checks_it() {
        let e = validate_workflow(&json!([1]), ValidateOptions::COMPLETE);
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!([{"path": "/", "code": "invalid_type", "message": "Workflow must be a JSON object", "expected": "object", "received": "list"}])
        );
        let e = validate_workflow(&json!({"nodes": "x"}), ValidateOptions::COMPLETE);
        let msgs: Vec<&str> = e.iter().map(|x| x.message.as_str()).collect();
        assert_eq!(
            msgs,
            vec![
                "Workflow needs a non-empty name",
                "Workflow needs a nodes array",
                "Workflow needs an edges array"
            ]
        );
        assert_eq!(e[1].received, Some(json!("str")));
        assert_eq!(e[2].received, None);
        assert!(
            validate_workflow(&json!({"nodes": [], "edges": []}), ValidateOptions::DRAFT)
                .is_empty()
        );
    }

    #[test]
    fn trigger_config_reads_only_registered_fields() {
        let a = json!([{"id": "s", "type": "start", "data": {"scheduleType": "daily", "time": "09:15", "label": "x"}},
                       {"id": "l", "type": "log", "data": {"message": "m"}}]);
        let a = a.as_array().unwrap();
        assert_eq!(
            trigger_config(a),
            json!({"start": {"scheduleType": "daily", "time": "09:15"}})
        );
        let b = json!([{"id": "s", "type": "start", "data": {"scheduleType": "daily", "time": "09:20"}}]);
        assert_ne!(trigger_config(a), trigger_config(b.as_array().unwrap()));
        assert_eq!(trigger_config(&[]), json!({}));
    }
}
