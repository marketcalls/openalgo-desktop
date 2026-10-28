//! A server's tools as `list_mcp_tools` shows them (the web's `_summary`,
//! `_argument_types` and `_compact_schema`): a one-line summary and the
//! argument names and types for the catalogue, and a compact schema for
//! one tool in full, so a 21-tool server fits one result.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_SUMMARY_CHARS: usize = 200;
pub const MAX_TOOL_DESCRIPTION_CHARS: usize = 2500;
pub const MAX_PROPERTY_DESCRIPTION_CHARS: usize = 300;

fn first_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// The first sentence or line of a description, capped.
pub fn summary(description: Option<&Value>) -> String {
    let text = description.and_then(Value::as_str).unwrap_or("").trim();
    let mut first = text.split('\n').next().unwrap_or("").trim();
    if let Some(end) = first.find(". ").filter(|e| *e > 0) {
        first = &first[..=end];
    }
    first_chars(first, MAX_SUMMARY_CHARS)
}

/// A tool's full description, capped.
pub fn description(description: Option<&Value>) -> String {
    first_chars(
        description.and_then(Value::as_str).unwrap_or("").trim(),
        MAX_TOOL_DESCRIPTION_CHARS,
    )
}

/// One argument as the model needs it, in the web's key order.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Property {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<Value>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub choices: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A tool's `inputSchema` shrunk to what calling it needs: each property's
/// type, enum, default, bounds, format and a capped description, and the
/// required list. `$schema`, titles and `additionalProperties` go.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CompactSchema {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, Property>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

fn property(prop: &serde_json::Map<String, Value>) -> Property {
    let get = |k: &str| prop.get(k).cloned();
    let mut p = Property {
        kind: get("type"),
        choices: get("enum"),
        default: get("default"),
        minimum: get("minimum"),
        maximum: get("maximum"),
        format: get("format"),
        description: prop
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(|d| first_chars(d, MAX_PROPERTY_DESCRIPTION_CHARS)),
    };
    if p.kind.is_none() {
        if let Some(Value::Array(any)) = prop.get("anyOf") {
            let types: Vec<Value> = any
                .iter()
                .filter_map(|i| i.get("type").cloned())
                .filter(|t| !t.is_null() && t != "")
                .collect();
            p.kind = Some(Value::Array(types));
        }
    }
    p
}

pub fn compact_schema(schema: Option<&Value>) -> CompactSchema {
    let Some(schema) = schema.and_then(Value::as_object) else {
        return CompactSchema::default();
    };
    let properties: BTreeMap<String, Property> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| {
            props
                .iter()
                .filter_map(|(name, p)| Some((name.clone(), property(p.as_object()?))))
                .collect()
        })
        .unwrap_or_default();
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .map(|n| n.as_str().map_or_else(|| n.to_string(), str::to_string))
                .collect()
        })
        .unwrap_or_default();
    CompactSchema {
        properties: (!properties.is_empty()).then_some(properties),
        required: (!required.is_empty()).then_some(required),
    }
}

/// Each argument's name and type, for the catalogue (`any` when unstated).
pub fn argument_types(schema: Option<&Value>) -> BTreeMap<String, Value> {
    compact_schema(schema)
        .properties
        .unwrap_or_default()
        .into_iter()
        .map(|(name, p)| (name, p.kind.unwrap_or_else(|| Value::from("any"))))
        .collect()
}
