//! A tool call's arguments, checked before the tool runs (the web's
//! `services/agent/tool_guard.py`, and what agno and pydantic do around
//! every tool call).
//!
//! The model's arguments are read as agno reads them (a JSON object; the
//! strings `none`, `null`, `true` and `false` mean what they say), then
//! checked against the tool's schema with pydantic's lax rules (`"5"` is
//! an integer, `5.0` is too, `"yes"` is a boolean). A call that does not
//! fit never reaches the tool: the model reads one correction naming each
//! missing argument, each wrong one with the reason, and each the tool
//! does not take, through the same channel as any refused value. When the
//! arguments arrived empty it also says so and suggests one call at a
//! time, the fallback when a provider drops a batch's arguments.

use serde_json::{Map, Number, Value};

/// What the model is told for a tool name it was not given (agno's text).
pub const UNKNOWN_TOOL: &str = "Error: The requested tool does not exist or is not available.";

/// What the model is told for arguments that are not a JSON object.
pub const NOT_AN_OBJECT: &str =
    "Function arguments are not a valid JSON object.\n\n Please fix and retry.";

/// Read the model's raw arguments into a mapping (agno's
/// `get_function_call`): a JSON object, or text holding one; nothing or an
/// empty list is no arguments.
pub fn read_arguments(raw: &Value) -> Result<Map<String, Value>, String> {
    let parsed = match raw {
        Value::Null => return Ok(Map::new()),
        Value::String(s) if s.trim().is_empty() => return Ok(Map::new()),
        Value::String(s) => serde_json::from_str::<Value>(s).map_err(|e| {
            format!(
                "Error while decoding function arguments: {}\n\nPlease make sure we can json.loads() the arguments and retry.",
                e
            )
        })?,
        other => other.clone(),
    };
    match parsed {
        Value::Array(a) if a.is_empty() => Ok(Map::new()),
        Value::Object(map) => Ok(map
            .into_iter()
            .map(|(k, v)| {
                let v = match &v {
                    Value::String(s) => match s.trim().to_lowercase().as_str() {
                        "none" | "null" => Value::Null,
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        _ => v,
                    },
                    _ => v,
                };
                (k, v)
            })
            .collect()),
        _ => Err(NOT_AN_OBJECT.into()),
    }
}

/// One refused argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Missing(String),
    Unexpected(String),
    Wrong { field: String, reason: String },
}

/// Check and coerce `args` against a tool's JSON schema. Returns the
/// coerced arguments, or every refusal.
pub fn validate(
    schema: &Value,
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, Vec<Refusal>> {
    let props = schema.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut refusals = Vec::new();
    let mut out = Map::new();
    for name in &required {
        if !args.contains_key(*name) {
            refusals.push(Refusal::Missing((*name).to_string()));
        }
    }
    for (name, value) in args {
        match props.and_then(|p| p.get(name)) {
            None => refusals.push(Refusal::Unexpected(name.clone())),
            Some(prop) => match coerce(prop, value, name) {
                Ok(v) => {
                    out.insert(name.clone(), v);
                }
                Err(mut wrong) => refusals.append(&mut wrong),
            },
        }
    }
    if refusals.is_empty() {
        Ok(out)
    } else {
        Err(refusals)
    }
}

fn wrong(field: &str, reason: &str) -> Vec<Refusal> {
    vec![Refusal::Wrong {
        field: field.into(),
        reason: reason.into(),
    }]
}

/// One value against one property schema (`type`, or `anyOf` tried in
/// order with an exact match preferred, as pydantic's smart unions do).
fn coerce(prop: &Value, value: &Value, field: &str) -> Result<Value, Vec<Refusal>> {
    if let Some(options) = prop.get("anyOf").and_then(Value::as_array) {
        if let Some(exact) = options.iter().find(|o| exact_type(o, value)) {
            return coerce(exact, value, field);
        }
        let mut errors = Vec::new();
        for option in options {
            let label = format!("{}.{}", field, type_label(option));
            match coerce(option, value, &label) {
                Ok(v) => return Ok(v),
                Err(mut e) => errors.append(&mut e),
            }
        }
        return Err(errors);
    }
    match prop.get("type").and_then(Value::as_str) {
        Some("string") => match value {
            Value::String(_) => Ok(value.clone()),
            _ => Err(wrong(field, "Input should be a valid string")),
        },
        Some("integer") => to_integer(value).map_err(|r| wrong(field, r)),
        Some("number") => to_number(value).map_err(|r| wrong(field, r)),
        Some("boolean") => to_bool(value).map_err(|r| wrong(field, r)),
        Some("null") => match value {
            Value::Null => Ok(Value::Null),
            _ => Err(wrong(field, "Input should be None")),
        },
        Some("array") => match value {
            Value::Array(items) => {
                let item_schema = prop.get("items");
                let mut out = Vec::new();
                let mut errors = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    match item_schema {
                        Some(s) => match coerce(s, item, &format!("{}.{}", field, i)) {
                            Ok(v) => out.push(v),
                            Err(mut e) => errors.append(&mut e),
                        },
                        None => out.push(item.clone()),
                    }
                }
                if errors.is_empty() {
                    Ok(Value::Array(out))
                } else {
                    Err(errors)
                }
            }
            _ => Err(wrong(field, "Input should be a valid list")),
        },
        Some("object") => match value {
            Value::Object(_) => Ok(value.clone()),
            _ => Err(wrong(field, "Input should be a valid dictionary")),
        },
        // No type stated: anything goes, as `Any` does.
        _ => Ok(value.clone()),
    }
}

fn type_label(option: &Value) -> &str {
    match option.get("type").and_then(Value::as_str) {
        Some("string") => "str",
        Some("integer") => "int",
        Some("number") => "float",
        Some("boolean") => "bool",
        Some("array") => "list",
        Some("object") => "dict",
        Some("null") => "none",
        _ => "any",
    }
}

fn exact_type(option: &Value, value: &Value) -> bool {
    match (option.get("type").and_then(Value::as_str), value) {
        (Some("string"), Value::String(_)) => true,
        (Some("integer"), Value::Number(n)) => n.is_i64() || n.is_u64(),
        (Some("number"), Value::Number(_)) => true,
        (Some("boolean"), Value::Bool(_)) => true,
        (Some("null"), Value::Null) => true,
        (Some("array"), Value::Array(_)) => true,
        (Some("object"), Value::Object(_)) => true,
        _ => false,
    }
}

fn to_integer(value: &Value) -> Result<Value, &'static str> {
    const PARSE: &str = "Input should be a valid integer, unable to parse string as an integer";
    const FRACTION: &str = "Input should be a valid integer, got a number with a fractional part";
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(value.clone()),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Ok(Value::from(f as i64)),
            _ => Err(FRACTION),
        },
        Value::Bool(b) => Ok(Value::from(i64::from(*b))),
        Value::String(s) => {
            let t = s.trim();
            if let Ok(i) = t.parse::<i64>() {
                return Ok(Value::from(i));
            }
            match t.parse::<f64>() {
                Ok(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 => {
                    Ok(Value::from(f as i64))
                }
                Ok(f) if f.is_finite() => Err(FRACTION),
                _ => Err(PARSE),
            }
        }
        _ => Err("Input should be a valid integer"),
    }
}

fn to_number(value: &Value) -> Result<Value, &'static str> {
    match value {
        Value::Number(_) => Ok(value.clone()),
        Value::Bool(b) => Ok(Value::from(if *b { 1.0 } else { 0.0 })),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .and_then(Number::from_f64)
            .map(Value::Number)
            .ok_or("Input should be a valid number, unable to parse string as a number"),
        _ => Err("Input should be a valid number"),
    }
}

fn to_bool(value: &Value) -> Result<Value, &'static str> {
    const PARSE: &str = "Input should be a valid boolean, unable to interpret input";
    match value {
        Value::Bool(_) => Ok(value.clone()),
        Value::Number(n) => {
            let f = n.as_f64();
            if f == Some(0.0) {
                Ok(Value::Bool(false))
            } else if f == Some(1.0) {
                Ok(Value::Bool(true))
            } else {
                Err(PARSE)
            }
        }
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Ok(Value::Bool(false)),
            "1" | "on" | "t" | "true" | "y" | "yes" => Ok(Value::Bool(true)),
            _ => Err(PARSE),
        },
        _ => Err("Input should be a valid boolean"),
    }
}

fn unique(items: Vec<String>) -> Vec<String> {
    let mut seen = Vec::new();
    for i in items {
        if !seen.contains(&i) {
            seen.push(i);
        }
    }
    seen
}

/// The correction the model reads instead of the tool's result (the web's
/// `tool_guard.correction`).
pub fn correction(tool: &str, refusals: &[Refusal], arguments: &Map<String, Value>) -> String {
    let mut missing = Vec::new();
    let mut unexpected = Vec::new();
    let mut wrong = Vec::new();
    for r in refusals {
        match r {
            Refusal::Missing(f) => missing.push(f.clone()),
            Refusal::Unexpected(f) => unexpected.push(f.clone()),
            Refusal::Wrong { field, reason } => wrong.push(format!("{} ({})", field, reason)),
        }
    }
    let empty = arguments.is_empty();
    let mut parts = vec![if empty {
        format!(
            "The call to {} was not run because its arguments did not arrive: it reached the tool with none at all.",
            tool
        )
    } else {
        format!(
            "The call to {} was not run because its arguments were refused.",
            tool
        )
    }];
    if !missing.is_empty() {
        parts.push(format!("Missing: {}.", unique(missing).join(", ")));
    }
    if !wrong.is_empty() {
        parts.push(format!(
            "Wrong type or value: {}.",
            unique(wrong).join("; ")
        ));
    }
    if !unexpected.is_empty() {
        parts.push(format!(
            "Not arguments of this tool: {}. Leave them out.",
            unique(unexpected).join(", ")
        ));
    }
    parts.push(format!(
        "Call {} again with every required argument filled in, using the names and formats in its description.",
        tool
    ));
    if empty {
        parts.push(
            "If several calls made together in one turn keep arriving empty, make them one at a time instead."
                .into(),
        );
    }
    parts.join(" ")
}

/// The required arguments a paused call does not carry, sorted; a value
/// sent as null or an empty string counts as missing (the approval card
/// refuses to approve while this is not empty).
pub fn missing_arguments(schema: &Value, arguments: &Map<String, Value>) -> Vec<String> {
    let mut missing: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|name| match arguments.get(*name) {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty(),
            _ => false,
        })
        .map(str::to_string)
        .collect();
    missing.sort();
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "symbol": {"type": "string"},
                "exchange": {"type": "string"},
                "days": {"type": "integer"},
                "order_id": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
                "price": {"anyOf": [{"type": "number"}, {"type": "null"}]},
                "symbols": {"type": "array", "items": {"type": "string"}},
                "flag": {"type": "boolean"},
            },
            "required": ["symbol", "exchange"],
            "additionalProperties": false
        })
    }

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn empty_arguments_become_a_named_correction() {
        let refused = validate(&schema(), &Map::new()).unwrap_err();
        let m = correction("get_history", &refused, &Map::new());
        assert!(m.contains("get_history") && m.contains("did not arrive"));
        assert!(m.contains("Missing: symbol, exchange."), "{}", m);
        assert!(m.ends_with("make them one at a time instead."));
    }

    #[test]
    fn a_wrong_type_and_an_unknown_argument_are_named() {
        let a = args(json!({"symbol": "TCS", "exchange": "NSE", "days": "five", "ticker": "x"}));
        let refused = validate(&schema(), &a).unwrap_err();
        let m = correction("get_history", &refused, &a);
        assert!(
            m.contains(
                "days (Input should be a valid integer, unable to parse string as an integer)"
            ),
            "{}",
            m
        );
        assert!(m.contains("Not arguments of this tool: ticker. Leave them out."));
        assert!(!m.contains("did not arrive"));
    }

    #[test]
    fn lax_values_are_coerced_as_pydantic_does() {
        let a = args(json!({
            "symbol": "TCS", "exchange": "NSE", "days": "5", "order_id": 123,
            "price": "101.5", "flag": "yes", "symbols": ["A"]
        }));
        let ok = validate(&schema(), &a).unwrap();
        assert_eq!(ok["days"], 5);
        assert_eq!(ok["order_id"], 123);
        assert_eq!(ok["price"], 101.5);
        assert_eq!(ok["flag"], true);
        assert!(validate(&schema(), &args(json!({"symbol": 5, "exchange": "NSE"}))).is_err());
        assert_eq!(
            validate(
                &schema(),
                &args(json!({"symbol": "A", "exchange": "B", "days": 2.5}))
            )
            .unwrap_err(),
            vec![Refusal::Wrong {
                field: "days".into(),
                reason: "Input should be a valid integer, got a number with a fractional part"
                    .into()
            }]
        );
    }

    #[test]
    fn arguments_are_read_as_agno_reads_them() {
        assert_eq!(read_arguments(&json!("")).unwrap(), Map::new());
        assert_eq!(read_arguments(&json!([])).unwrap(), Map::new());
        let a = read_arguments(&json!("{\"price\":\"null\",\"x\":\"TRUE\"}")).unwrap();
        assert_eq!(a["price"], Value::Null);
        assert_eq!(a["x"], true);
        assert_eq!(read_arguments(&json!([1])).unwrap_err(), NOT_AN_OBJECT);
        assert!(read_arguments(&json!("{oops"))
            .unwrap_err()
            .starts_with("Error while decoding"));
    }

    #[test]
    fn null_and_empty_count_as_missing_and_no_argument_tools_are_complete() {
        let order = json!({"required": ["symbol", "exchange", "action", "quantity"]});
        assert_eq!(
            missing_arguments(
                &order,
                &args(json!({"symbol": "", "exchange": null, "action": "BUY", "quantity": 1}))
            ),
            vec!["exchange", "symbol"]
        );
        assert!(
            missing_arguments(&json!({"type": "object", "properties": {}}), &Map::new()).is_empty()
        );
    }
}
