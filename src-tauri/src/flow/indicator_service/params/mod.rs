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
//! | `text` | `json.loads` of the params text with Python's key order and `NaN` |

use serde_json::{Map, Value};

use super::IndicatorError;
use crate::agent::indicators::ta::{ParamValue, Params};
use crate::agent::indicators::{required_default, IndicatorSpec, Kind, ParamDefault};
use crate::flow::contract::pyfmt::{self, truthy};

mod python;
mod rules;
mod text;

use python::Py;
use text::Loaded;

/// The node's parameters, keys in the order the trader wrote them (a Python
/// dict keeps insertion order; a `serde_json` map sorts its keys). A value
/// the text wrote as `NaN` or `Infinity` is a float to the method; JSON
/// cannot hold it, so it is kept as its Python text and listed in
/// `non_finite`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndicatorParams {
    pairs: Vec<(String, Value)>,
    non_finite: Vec<String>,
}

impl IndicatorParams {
    /// Parameters from pairs, in the order given (a repeated key keeps its
    /// first place and its last value, as a Python dict does).
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, Value)>) -> Self {
        let mut params = Self::default();
        for (key, value) in pairs {
            params.insert(key, value, false);
        }
        params
    }

    /// Parameters from an object already parsed: the legacy object form the
    /// executor normalises (its keys arrive sorted).
    pub fn from_map(map: &Map<String, Value>) -> Self {
        Self::from_pairs(map.iter().map(|(k, v)| (k.clone(), v.clone())))
    }

    /// `json.loads(params_raw) if params_raw.strip() else {}` (Python's
    /// `NaN` and `Infinity` included) and the two refusals the executor
    /// answers with, word for word.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        match text::loads(raw) {
            None => Err(format!("Invalid params JSON: {raw}")),
            Some(Loaded::Other(kind)) => {
                Err(format!("params must resolve to a JSON object, got {kind}"))
            }
            Some(Loaded::Object(members)) => {
                let mut params = Self::default();
                for (key, value, float) in members {
                    params.insert(key, value, float);
                }
                Ok(params)
            }
        }
    }

    /// The pairs, in order.
    pub fn pairs(&self) -> &[(String, Value)] {
        &self.pairs
    }

    /// The parameters as a JSON object (keys sorted, as every map here is;
    /// a non-finite float as its Python text).
    pub fn to_value(&self) -> Value {
        Value::Object(self.pairs.iter().cloned().collect())
    }

    /// The float `key` holds when the text wrote it as `NaN`, `Infinity`
    /// or a number past any float.
    pub(crate) fn non_finite(&self, key: &str) -> Option<f64> {
        if !self.non_finite.iter().any(|k| k == key) {
            return None;
        }
        let text = self.pairs.iter().find(|(k, _)| k == key)?.1.as_str()?;
        Some(match text {
            "nan" => f64::NAN,
            "-inf" => f64::NEG_INFINITY,
            _ => f64::INFINITY,
        })
    }

    fn insert(&mut self, key: String, value: Value, float: bool) {
        self.non_finite.retain(|k| *k != key);
        if float {
            self.non_finite.push(key.clone());
        }
        match self.pairs.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.pairs.push((key, value)),
        }
    }

    /// `_resolve_params`: the caller's parameters, then each required one
    /// the caller left out at its `REQUIRED_PARAM_DEFAULTS` value.
    pub(crate) fn resolved(&self, name: &str) -> Self {
        let mut out = self.clone();
        if let Some((key, value)) = required_default(name) {
            if !out.pairs.iter().any(|(k, _)| k == key) {
                out.pairs.push((key.to_string(), Value::from(value)));
            }
        }
        out
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
    let mut read = match params.non_finite(param) {
        Some(float) => Py::Float(float),
        None => Py::from_json(value),
    };
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
                pyfmt::repr(value)
            )))
        }
    })
}
