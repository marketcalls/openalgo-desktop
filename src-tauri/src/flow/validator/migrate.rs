//! Two helpers the routes call around the validator: the import-time
//! upgrade of legacy node payloads (`migrate_legacy_node_data`), and the
//! activation-relevant configuration of a graph's trigger
//! (`trigger_config`), compared before and after a change to know whether
//! the workflow must be re-registered.

use super::catalog::{is_one_of, TRIGGER_NODE_TYPES};
use super::py;
use super::structure::get;
use serde::Serialize;
use serde_json::{Map, Value};

/// Upgrade legacy node payloads, so an older exported workflow is stored in
/// its canonical shape rather than with a field no reader honours. Returns
/// the nodes and one human-readable note per change (or per change refused).
pub fn migrate_legacy_node_data(nodes: Vec<Value>) -> (Vec<Value>, Vec<String>) {
    let mut notes = Vec::new();
    let migrated = nodes
        .into_iter()
        .map(|mut node| {
            let Some(object) = node.as_object_mut() else {
                return node;
            };
            let node_type = get(object, "type").as_str().unwrap_or_default().to_string();
            let id = py::str_of(get(object, "id"));
            if let Some(data) = object.get_mut("data").and_then(Value::as_object_mut) {
                if let Some(note) = migrate_data(&node_type, &id, data) {
                    notes.push(note);
                }
            }
            node
        })
        .collect();
    (migrated, notes)
}

fn migrate_data(node_type: &str, id: &str, data: &mut Map<String, Value>) -> Option<String> {
    match node_type {
        "fundCheck" if !data.contains_key("minAvailable") && data.contains_key("threshold") => {
            Some(migrate_fund_check(id, data))
        }
        "positionCheck" if !data.contains_key("condition") && data.contains_key("operator") => {
            Some(migrate_position_check(id, data))
        }
        "priceCondition" if !data.contains_key("value") && data.contains_key("threshold") => {
            let threshold = data.remove("threshold").unwrap_or_default();
            data.insert("value".to_string(), threshold);
            Some(format!("priceCondition '{id}': threshold -> value"))
        }
        _ => None,
    }
}

/// `operator or default`, trimmed and lower-cased, as the web reads it.
fn legacy_operator(data: &Map<String, Value>, default: &str) -> String {
    let operator = get(data, "operator");
    let text = if py::truthy(operator) {
        py::str_of(operator)
    } else {
        default.to_string()
    };
    py::strip(&text).to_lowercase()
}

/// fundCheck can only express "at least this much". A legacy greater-than
/// maps cleanly; a legacy less-than means the opposite, and silently
/// reversing a capital guard is worse than refusing to guess.
fn migrate_fund_check(id: &str, data: &mut Map<String, Value>) -> String {
    let operator = legacy_operator(data, "gt");
    if matches!(operator.as_str(), "gt" | "gte" | "ge" | ">" | ">=") {
        let threshold = data.remove("threshold").unwrap_or_default();
        let shown = py::str_of(&threshold);
        data.insert("minAvailable".to_string(), threshold);
        data.remove("operator");
        format!("fundCheck '{id}': threshold -> minAvailable ({shown})")
    } else {
        format!(
            "fundCheck '{id}': operator '{operator}' has no equivalent - this node only supports a \
             minimum. Left unchanged; set Minimum Available yourself."
        )
    }
}

/// How a legacy positionCheck operator maps onto the node's conditions.
#[derive(Clone, Copy)]
enum LegacyComparison {
    /// `>` or `<`: the condition compares the same way.
    Strict(&'static str),
    /// `>=`: `quantity_above` a threshold one lower.
    AtLeast,
    /// `<=`: `quantity_below` a threshold one higher.
    AtMost,
}

/// Keep the comparison rather than flattening every legacy operator to
/// "exists", which would turn a quantity-based exit into an unconditional one.
///
/// The node's `quantity_above` and `quantity_below` are strict, and it reads
/// the position's quantity as a whole number, so an inclusive legacy
/// operator keeps its meaning by moving the threshold: `qty >= t` is
/// `qty > ceil(t) - 1` and `qty <= t` is `qty < floor(t) + 1`. The web maps
/// `>=` straight onto the strict condition, so a legacy "quantity at least 1"
/// stops passing at exactly 1 (parity V-113, a web defect fixed by lead
/// decision 23). A threshold that is not a number cannot be moved, so that
/// node is left for the trader to restate.
fn migrate_position_check(id: &str, data: &mut Map<String, Value>) -> String {
    let operator = legacy_operator(data, "");
    let comparison = match operator.as_str() {
        "gt" | ">" => Some(LegacyComparison::Strict("quantity_above")),
        "lt" | "<" => Some(LegacyComparison::Strict("quantity_below")),
        "gte" | "ge" | ">=" => Some(LegacyComparison::AtLeast),
        "lte" | "le" | "<=" => Some(LegacyComparison::AtMost),
        _ => None,
    };
    let unchanged = || {
        format!(
            "positionCheck '{id}': operator '{operator}' has no equivalent condition. Left \
             unchanged; set the condition yourself."
        )
    };
    let (condition, moved) = match comparison {
        None => return unchanged(),
        Some(LegacyComparison::Strict(condition)) => (condition, None),
        Some(inclusive) => {
            let Some(t) = py::float_of(get(data, "threshold")).filter(|t| t.is_finite()) else {
                return unchanged();
            };
            match inclusive {
                LegacyComparison::AtLeast => ("quantity_above", Some(t.ceil() - 1.0)),
                _ => ("quantity_below", Some(t.floor() + 1.0)),
            }
        }
    };
    data.insert("condition".to_string(), Value::from(condition));
    data.remove("operator");
    let Some(threshold) = moved else {
        return format!(
            "positionCheck '{id}': operator '{operator}' -> condition '{condition}' (threshold kept)"
        );
    };
    let was = py::str_of(get(data, "threshold"));
    // A whole number within any position size; `as` saturates past i64.
    let now = threshold as i64;
    data.insert("threshold".to_string(), Value::from(now));
    format!(
        "positionCheck '{id}': operator '{operator}' -> condition '{condition}' (threshold {was} \
         -> {now}, which keeps '{operator}' on whole-number quantities)"
    )
}

/// Trigger fields registered with the scheduler or a monitor at activation
/// rather than read per run. A change to any of them needs the workflow
/// re-registered, which comparing node types alone would miss: editing an
/// interval from 1m to 5m keeps the same trigger type.
pub const TRIGGER_CONFIG_FIELDS: [&str; 19] = [
    // start
    "scheduleType",
    "time",
    "days",
    "executeAt",
    "intervalMinutes",
    "intervalValue",
    "intervalUnit",
    "marketHoursOnly",
    // priceAlert: every field the monitor captures, including the channel
    // bounds and percentage that only some conditions use
    "symbol",
    "exchange",
    "condition",
    "price",
    "priceLower",
    "priceUpper",
    "percentage",
    "expiration",
    // orderUpdateTrigger
    "orderId",
    "status",
    // shared
    "trigger",
];

/// The activation-relevant configuration of a graph's trigger node(s):
/// `{trigger type: {field: value}}`. Two graphs with equal configurations
/// can be swapped under a running workflow; anything else needs it
/// re-registered. Compares as Python's `==` does (`1 == 1.0 == true`).
/// Serializes with serde_json's (sorted) key order, where the web keeps the
/// order the fields are listed in.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(transparent)]
pub struct TriggerConfig(Map<String, Value>);

impl PartialEq for TriggerConfig {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .all(|(key, value)| other.0.get(key).is_some_and(|o| py::eq(value, o)))
    }
}

/// The trigger configuration of `nodes` (the web's `trigger_config`). A
/// later trigger of the same type replaces an earlier one, as in the web's
/// dict.
pub fn trigger_config(nodes: &[Value]) -> TriggerConfig {
    let mut config = Map::new();
    for node in nodes.iter().filter_map(Value::as_object) {
        let node_type = get(node, "type");
        if !node_type
            .as_str()
            .is_some_and(|t| is_one_of(&TRIGGER_NODE_TYPES, t))
        {
            continue;
        }
        let data = get(node, "data");
        let fields = match data {
            Value::Object(fields) => fields.clone(),
            other if !py::truthy(other) => Map::new(),
            _ => continue,
        };
        let captured: Map<String, Value> = TRIGGER_CONFIG_FIELDS
            .iter()
            .filter_map(|f| fields.get(*f).map(|v| ((*f).to_string(), v.clone())))
            .collect();
        config.insert(py::str_of(node_type), Value::Object(captured));
    }
    TriggerConfig(config)
}
