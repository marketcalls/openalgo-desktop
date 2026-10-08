//! Tool parameter types and the JSON Schema the web's FastMCP builds from
//! them (pydantic: `title` from the parameter name, `anyOf [T, null]` for an
//! optional `T | None`, the default as written in the Python signature), plus
//! argument checking for a call.

use serde_json::{json, Map, Value};

/// A parameter's Python type, as it shows in the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    /// `str` (also `str = None`, which pydantic renders without `anyOf`).
    Str,
    /// `int`
    Int,
    /// `float`
    Num,
    /// `bool`
    Bool,
    /// `list[dict[str, Any]]`
    ObjectList,
    /// `list[dict[str, str]]`
    StrMapList,
    /// `str | None`
    OptStr,
    /// `int | None`
    OptInt,
    /// `float | None`
    OptNum,
    /// `dict[str, Any] | None`
    OptObject,
    /// `list[str] | None`
    OptStrList,
}

/// A parameter's default in the Python signature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dflt {
    Required,
    Null,
    Str(&'static str),
    Int(i64),
    Num(f64),
}

#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub name: &'static str,
    pub ty: Ty,
    pub default: Dflt,
}

/// pydantic's field title: words split on `_`, each capitalised.
pub fn title(name: &str) -> String {
    name.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut out = String::new();
            let mut upper_next = true;
            // Python `str.title()`: a letter after a non-letter starts a word.
            for c in w.chars() {
                if c.is_alphabetic() {
                    if upper_next {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    upper_next = false;
                } else {
                    out.push(c);
                    upper_next = true;
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn type_schema(ty: Ty) -> Value {
    match ty {
        Ty::Str => json!({"type": "string"}),
        Ty::Int => json!({"type": "integer"}),
        Ty::Num => json!({"type": "number"}),
        Ty::Bool => json!({"type": "boolean"}),
        Ty::ObjectList => json!({
            "items": {"additionalProperties": true, "type": "object"},
            "type": "array"
        }),
        Ty::StrMapList => json!({
            "items": {"additionalProperties": {"type": "string"}, "type": "object"},
            "type": "array"
        }),
        Ty::OptStr => json!({"anyOf": [{"type": "string"}, {"type": "null"}]}),
        Ty::OptInt => json!({"anyOf": [{"type": "integer"}, {"type": "null"}]}),
        Ty::OptNum => json!({"anyOf": [{"type": "number"}, {"type": "null"}]}),
        Ty::OptObject => {
            json!({"anyOf": [{"additionalProperties": true, "type": "object"}, {"type": "null"}]})
        }
        Ty::OptStrList => {
            json!({"anyOf": [{"items": {"type": "string"}, "type": "array"}, {"type": "null"}]})
        }
    }
}

/// The default as a JSON value (`None` for a required parameter).
pub fn default_value(d: Dflt) -> Option<Value> {
    match d {
        Dflt::Required => None,
        Dflt::Null => Some(Value::Null),
        Dflt::Str(s) => Some(json!(s)),
        Dflt::Int(i) => Some(json!(i)),
        Dflt::Num(f) => Some(json!(f)),
    }
}

/// The `inputSchema` FastMCP publishes for a tool.
pub fn input_schema(tool: &str, params: &[Param]) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for p in params {
        let mut s = type_schema(p.ty);
        if let Some(obj) = s.as_object_mut() {
            obj.insert("title".into(), json!(title(p.name)));
            match default_value(p.default) {
                Some(d) => {
                    obj.insert("default".into(), d);
                }
                None => required.push(json!(p.name)),
            }
        }
        props.insert(p.name.into(), s);
    }
    let mut schema = Map::new();
    schema.insert("properties".into(), Value::Object(props));
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required));
    }
    schema.insert("title".into(), json!(format!("{}Arguments", tool)));
    schema.insert("type".into(), json!("object"));
    Value::Object(schema)
}

/// Why a call's arguments were refused (the web answers every such case as
/// `bad_arguments`: the call raised `TypeError`).
#[derive(Debug, Clone, PartialEq)]
pub struct BadArguments(pub String);

fn int_like(v: &Value) -> bool {
    match v {
        Value::Number(n) => {
            n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        Value::String(s) => s.trim().parse::<i64>().is_ok(),
        _ => false,
    }
}

fn num_like(v: &Value) -> bool {
    match v {
        Value::Number(_) => true,
        Value::String(s) => s.trim().parse::<f64>().is_ok(),
        _ => false,
    }
}

fn fits(ty: Ty, v: &Value) -> bool {
    let opt = |inner: &dyn Fn(&Value) -> bool| v.is_null() || inner(v);
    match ty {
        Ty::Str => v.is_string() || v.is_null(),
        Ty::Int => int_like(v),
        Ty::Num => num_like(v),
        Ty::Bool => v.is_boolean(),
        Ty::ObjectList => v.as_array().is_some_and(|a| a.iter().all(Value::is_object)),
        Ty::StrMapList => v.as_array().is_some_and(|a| {
            a.iter().all(|i| {
                i.as_object()
                    .is_some_and(|o| o.values().all(Value::is_string))
            })
        }),
        Ty::OptStr => opt(&|x| x.is_string()),
        Ty::OptInt => opt(&int_like),
        Ty::OptNum => opt(&num_like),
        Ty::OptObject => opt(&|x| x.is_object()),
        Ty::OptStrList => opt(&|x| x.as_array().is_some_and(|a| a.iter().all(Value::is_string))),
    }
}

/// Bind call arguments to the parameters: every required one present, no
/// unknown names, every value of the declared kind (numbers may arrive as
/// numeric strings, as pydantic's lax mode accepts). Defaults fill the rest.
pub fn bind(
    params: &[Param],
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, BadArguments> {
    for k in args.keys() {
        if !params.iter().any(|p| p.name == k) {
            return Err(BadArguments(format!("unexpected argument '{}'", k)));
        }
    }
    let mut out = Map::new();
    for p in params {
        match args.get(p.name) {
            Some(v) => {
                if !fits(p.ty, v) {
                    return Err(BadArguments(format!(
                        "argument '{}' has the wrong type",
                        p.name
                    )));
                }
                out.insert(p.name.into(), v.clone());
            }
            None => match default_value(p.default) {
                Some(d) => {
                    out.insert(p.name.into(), d);
                }
                None => {
                    return Err(BadArguments(format!(
                        "missing required argument '{}'",
                        p.name
                    )))
                }
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_follow_pydantic() {
        assert_eq!(title("price_type"), "Price Type");
        assert_eq!(title("symbol1"), "Symbol1");
        assert_eq!(title("lookback_bars"), "Lookback Bars");
        assert_eq!(title("mode"), "Mode");
    }

    #[test]
    fn binding_checks_names_kinds_and_defaults() {
        let ps = [
            Param {
                name: "symbol",
                ty: Ty::Str,
                default: Dflt::Required,
            },
            Param {
                name: "quantity",
                ty: Ty::Int,
                default: Dflt::Required,
            },
            Param {
                name: "price",
                ty: Ty::OptNum,
                default: Dflt::Null,
            },
            Param {
                name: "exchange",
                ty: Ty::Str,
                default: Dflt::Str("NSE"),
            },
        ];
        let ok = bind(
            &ps,
            json!({"symbol": "SBIN", "quantity": "5"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(ok["exchange"], "NSE");
        assert_eq!(ok["price"], Value::Null);
        assert!(bind(&ps, json!({"symbol": "SBIN"}).as_object().unwrap()).is_err());
        assert!(bind(
            &ps,
            json!({"symbol": "SBIN", "quantity": 1, "x": 1})
                .as_object()
                .unwrap()
        )
        .is_err());
        assert!(bind(
            &ps,
            json!({"symbol": "SBIN", "quantity": "one"})
                .as_object()
                .unwrap()
        )
        .is_err());
        assert!(bind(
            &ps,
            json!({"symbol": "SBIN", "quantity": 1.5})
                .as_object()
                .unwrap()
        )
        .is_err());
    }
}
