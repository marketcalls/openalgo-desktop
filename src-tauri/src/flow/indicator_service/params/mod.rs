//! The Indicator node's `params`: read as the web's executor reads them,
//! filled with `REQUIRED_PARAM_DEFAULTS`, and bound to the library the way
//! Python binds `fn(*series, **params)`.
//!
//! The web passes each value to `openalgo.ta` untouched, so what a value of
//! any type does depends on the method's first operations on it: whether it
//! computes, with which number, or which exception it raises, word for word.
//! `python` models those operations and `rules` says which ones each
//! parameter meets, measured on the pinned openalgo build.
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `python` | a value as Python holds it, and each operation's semantics and sentences |
//! | `rules` | the operations each parameter goes through, mode-dependent ones included |

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::{Map, Value};

use super::IndicatorError;
use crate::agent::indicators::ta::{ParamValue, Params};
use crate::agent::indicators::{required_default, IndicatorSpec, Kind, ParamDefault};
use crate::agent::tools::values::{py_repr, truthy, type_name};

mod python;
mod rules;

use python::Py;

/// The node's parameters, keys in the order the trader wrote them (a Python
/// dict keeps insertion order; a `serde_json` map sorts its keys).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndicatorParams(Vec<(String, Value)>);

impl IndicatorParams {
    /// Parameters from pairs, in the order given (a repeated key keeps its
    /// first place and its last value, as a Python dict does).
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, Value)>) -> Self {
        let mut params = Self::default();
        for (key, value) in pairs {
            params.insert(key, value);
        }
        params
    }

    /// Parameters from an object already parsed: the legacy object form the
    /// executor normalises (its keys arrive sorted).
    pub fn from_map(map: &Map<String, Value>) -> Self {
        Self::from_pairs(map.iter().map(|(k, v)| (k.clone(), v.clone())))
    }

    /// `json.loads(params_raw) if params_raw.strip() else {}` and the two
    /// refusals the executor answers with, word for word.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        let invalid = || format!("Invalid params JSON: {raw}");
        let parsed: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
        if !parsed.is_object() {
            return Err(format!(
                "params must resolve to a JSON object, got {}",
                type_name(&parsed)
            ));
        }
        let ordered: Ordered = serde_json::from_str(raw).map_err(|_| invalid())?;
        Ok(ordered.0)
    }

    /// The pairs, in order.
    pub fn pairs(&self) -> &[(String, Value)] {
        &self.0
    }

    /// The parameters as a JSON object (keys sorted, as every map here is).
    pub fn to_value(&self) -> Value {
        Value::Object(self.0.iter().cloned().collect())
    }

    fn insert(&mut self, key: String, value: Value) {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key, value)),
        }
    }

    /// `_resolve_params`: the caller's parameters, then each required one
    /// the caller left out at its `REQUIRED_PARAM_DEFAULTS` value.
    pub(crate) fn resolved(&self, name: &str) -> Self {
        let mut out = self.clone();
        if let Some((key, value)) = required_default(name) {
            if !out.0.iter().any(|(k, _)| k == key) {
                out.0.push((key.to_string(), Value::from(value)));
            }
        }
        out
    }
}

/// A JSON object read with its keys in written order.
struct Ordered(IndicatorParams);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut params = IndicatorParams::default();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    params.insert(key, value);
                }
                Ok(Ordered(params))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

/// Bind `params` (already resolved) to `spec`'s signature, as
/// `fn(*series, **params)` binds them, and read each value as the method
/// would (`python`, `rules`). `bars` is the series length a NumPy operation
/// on a value broadcasts against.
pub(crate) fn bind(
    spec: &'static IndicatorSpec,
    params: &IndicatorParams,
    bars: usize,
) -> Result<Vec<(&'static str, ParamValue)>, IndicatorError> {
    // Python checks the keywords in the order written, before the method
    // runs: a series argument already given by position, then a name the
    // signature does not have.
    for (key, _) in params.pairs() {
        if spec.series_args.contains(&key.as_str()) {
            return Err(IndicatorError::type_error(format!(
                "TechnicalAnalysis.{}() got multiple values for argument '{}'",
                spec.name, key
            )));
        }
        if spec.param(key).is_none() {
            return Err(IndicatorError::type_error(format!(
                "TechnicalAnalysis.{}() got an unexpected keyword argument '{}'",
                spec.name, key
            )));
        }
    }
    // The method then reads its arguments in signature order.
    let mut bound = Vec::new();
    for p in spec.params {
        let Some((_, value)) = params.pairs().iter().find(|(k, _)| k == p.name) else {
            continue;
        };
        if let Some(read) = library_value(spec, p.name, p.kind, value, params, bars)? {
            bound.push((p.name, read));
        }
    }
    Ok(bound)
}

/// The method's arguments as the library is called with them: the bound
/// values, and the signature's own default for every parameter left out.
pub(crate) fn library_params(
    spec: &'static IndicatorSpec,
    bound: &[(&'static str, ParamValue)],
) -> Params {
    let values = spec
        .params
        .iter()
        .filter_map(|p| {
            if let Some((_, value)) = bound.iter().find(|(name, _)| *name == p.name) {
                return Some((p.name, value.clone()));
            }
            match p.default {
                ParamDefault::Required => None,
                ParamDefault::Int(v) => Some((p.name, ParamValue::Int(v))),
                ParamDefault::Float(v) => Some((p.name, ParamValue::Float(v))),
                ParamDefault::Str(v) => Some((p.name, ParamValue::Str(v.into()))),
                ParamDefault::Bool(v) => Some((p.name, ParamValue::Bool(v))),
            }
        })
        .collect();
    Params::new(spec.name, values).with_series(spec.series_args)
}

/// One value as the method reads it: what the library receives, `None`
/// where the library's default stands, or the exception the method raises.
fn library_value(
    spec: &IndicatorSpec,
    param: &str,
    kind: Kind,
    value: &Value,
    params: &IndicatorParams,
    bars: usize,
) -> Result<Option<ParamValue>, IndicatorError> {
    // A flag is read for its truth value, whatever it holds.
    if kind == Kind::Bool {
        return Ok(Some(ParamValue::Bool(truthy(value))));
    }
    let mut read = Py::from_json(value);
    for step in rules::steps_for(spec.name, param, params).unwrap_or(&[]) {
        read = python::apply(*step, read, bars)?;
    }
    Ok(match read {
        Py::Default => None,
        Py::Int(v) => Some(ParamValue::Int(v)),
        Py::Float(v) => Some(ParamValue::Float(v)),
        Py::Bool(v) => Some(ParamValue::Bool(v)),
        Py::Str(v) => Some(ParamValue::Str(v)),
        // No method's steps let these through to the library; an int past
        // 64 bits is the one value the library port cannot hold.
        Py::Big(_) | Py::None | Py::List(_) | Py::Dict(_) => {
            return Err(IndicatorError::value(format!(
                "{}: {} = {} is outside what this indicator can compute",
                spec.name,
                param,
                py_repr(value)
            )))
        }
    })
}
