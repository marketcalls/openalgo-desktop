//! The Indicator node's `params`: read as the web's executor reads them,
//! filled with `REQUIRED_PARAM_DEFAULTS`, and bound to the library the way
//! Python binds `fn(*series, **params)`.
//!
//! The web passes each value to `openalgo.ta` untouched, so what a value of
//! the wrong type does depends on the method. The cases a workflow meets are
//! reproduced from measurements on the pinned build: an integer where a
//! float is declared computes; a float where an integer is declared computes
//! in most methods; the period-like arguments `validate_period` checks
//! refuse anything but an integer with the library's own `TypeError`, as do
//! the four that pass a float to `range()` or a slice. Any other value of the
//! wrong type (text or null for a number, a number for a text parameter)
//! stops the run, as it does on the web for most methods, but with a sentence
//! a trader can act on instead of the interpreter's (a deliberate deviation,
//! recorded in the lane report).

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::{Map, Value};

use super::IndicatorError;
use crate::agent::indicators::ta::{ParamValue, Params};
use crate::agent::indicators::{required_default, IndicatorSpec, Kind, ParamDefault};
use crate::agent::tools::values::{py_repr, truthy, type_name};

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

/// The integer parameters `BaseIndicator.validate_period` checks first:
/// anything that is not an int (a bool is one) is refused with its
/// `TypeError`, "Period must be an integer, got <class '...'>". Measured on
/// openalgo 2.0.5 (`flow_indicators/params.json` pins the list).
const PERIOD_CHECKED: &[(&str, &[&str])] = &[
    ("adx", &["period"]),
    ("alma", &["period"]),
    ("aroon", &["period"]),
    ("aroon_oscillator", &["period"]),
    ("atr", &["period"]),
    ("bbands", &["period"]),
    ("bbpercent", &["period"]),
    ("bbwidth", &["period"]),
    ("cci", &["period"]),
    ("chandelier_exit", &["period"]),
    ("chop", &["period"]),
    ("ckstop", &["p", "q"]),
    ("cmf", &["period"]),
    ("cmo", &["period"]),
    ("crsi", &["lenroc"]),
    ("dema", &["period"]),
    ("dmi", &["period"]),
    ("donchian", &["period"]),
    ("dpo", &["period"]),
    ("ema", &["period"]),
    ("emv", &["length"]),
    ("fisher", &["length"]),
    ("force_index", &["length"]),
    ("frama", &["period"]),
    ("hma", &["period"]),
    ("hv", &["length"]),
    ("kama", &["length"]),
    ("keltner", &["ema_period", "atr_period"]),
    ("linreg", &["period"]),
    ("lrslope", &["period"]),
    ("ma_envelopes", &["period"]),
    ("mcginley", &["period"]),
    ("median", &["period"]),
    ("mfi", &["period"]),
    ("mode", &["period"]),
    ("rsi", &["period"]),
    ("rvi", &["period"]),
    ("rvol", &["period"]),
    ("rwi", &["period"]),
    ("sma", &["period"]),
    ("stochastic", &["k_period"]),
    ("stochrsi", &["rsi_period", "stoch_period"]),
    ("supertrend", &["period"]),
    ("t3", &["period"]),
    ("tema", &["period"]),
    ("trima", &["period"]),
    ("trix", &["length"]),
    ("tsf", &["period"]),
    ("variance", &["lookback"]),
    ("vidya", &["period"]),
    ("vroc", &["period"]),
    ("vwma", &["period"]),
    ("williams_r", &["period"]),
    ("wma", &["period"]),
    ("zlema", &["period"]),
];

/// Integer parameters a float reaches `range()` or a slice with, and the
/// `TypeError` that raises.
const FLOAT_TYPE_ERRORS: &[(&str, &str, &str)] = &[
    ("change", "length", INTEGER),
    ("falling", "length", INTEGER),
    ("rising", "length", INTEGER),
    ("ichimoku", "displacement", SLICE),
];

const INTEGER: &str = "'float' object cannot be interpreted as an integer";
const SLICE: &str = "slice indices must be integers or None or have an __index__ method";

/// Integer parameters the web's method uses as a number, so a fractional
/// value computes there with that fraction, while the library port reads
/// them as integers. A fractional value is refused here rather than
/// computed to different numbers (a deliberate deviation; whole values,
/// `365` or `365.0`, match the web). The editor's fields only write whole
/// numbers for these.
const FRACTION_REFUSED: &[(&str, &str)] = &[
    ("hv", "annual"),
    ("hv", "per"),
    ("lrslope", "interval"),
    ("pvi_with_signal", "signal_length"),
];

fn period_checked(method: &str, param: &str) -> bool {
    PERIOD_CHECKED
        .iter()
        .any(|(name, params)| *name == method && params.contains(&param))
}

fn float_type_error(method: &str, param: &str) -> Option<&'static str> {
    FLOAT_TYPE_ERRORS
        .iter()
        .find(|(name, p, _)| *name == method && *p == param)
        .map(|(_, _, message)| *message)
}

/// Bind `params` (already resolved) to `spec`'s signature, as
/// `fn(*series, **params)` binds them, and read each value as the library
/// receives it.
pub(crate) fn bind(
    spec: &'static IndicatorSpec,
    params: &IndicatorParams,
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
    params
        .pairs()
        .iter()
        .filter_map(|(key, value)| spec.param(key).map(|p| (p, value)))
        .map(|(p, value)| Ok((p.name, library_value(spec, p.name, p.kind, value)?)))
        .collect()
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

/// One value as the library receives it.
fn library_value(
    spec: &IndicatorSpec,
    param: &'static str,
    kind: Kind,
    value: &Value,
) -> Result<ParamValue, IndicatorError> {
    match (kind, value) {
        // A flag is read for its truth value, whatever it holds.
        (Kind::Bool, Value::Number(_) | Value::String(_) | Value::Bool(_)) => {
            Ok(text_or_number(value))
        }
        (Kind::Bool, other) => Ok(ParamValue::Bool(truthy(other))),
        (Kind::Str, Value::String(text)) => Ok(ParamValue::Str(text.clone())),
        // A Python bool is an int: True works wherever 1 does.
        (Kind::Int | Kind::Float, Value::Bool(flag)) => Ok(ParamValue::Bool(*flag)),
        (Kind::Int | Kind::Float, Value::Number(number)) if number.is_i64() => {
            Ok(ParamValue::Int(number.as_i64().unwrap_or_default()))
        }
        (Kind::Int, other) if period_checked(spec.name, param) && !other.is_u64() => {
            Err(IndicatorError::type_error(format!(
                "Period must be an integer, got <class '{}'>",
                type_name(other)
            )))
        }
        (Kind::Int | Kind::Float, Value::Number(number)) if number.is_f64() => {
            let float = number.as_f64().unwrap_or(f64::NAN);
            if kind == Kind::Int {
                if let Some(message) = float_type_error(spec.name, param) {
                    return Err(IndicatorError::type_error(message));
                }
                if float.fract() != 0.0 && FRACTION_REFUSED.contains(&(spec.name, param)) {
                    return Err(mistyped(spec, param, kind, value));
                }
            }
            Ok(ParamValue::Float(float))
        }
        _ => Err(mistyped(spec, param, kind, value)),
    }
}

fn text_or_number(value: &Value) -> ParamValue {
    match value {
        Value::Bool(flag) => ParamValue::Bool(*flag),
        Value::String(text) => ParamValue::Str(text.clone()),
        Value::Number(n) => match n.as_i64() {
            Some(int) => ParamValue::Int(int),
            None => ParamValue::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        other => ParamValue::Bool(truthy(other)),
    }
}

/// A value of the wrong type: the run stops, as it does on the web, with a
/// sentence that names the field and what it needs.
fn mistyped(spec: &IndicatorSpec, param: &str, kind: Kind, value: &Value) -> IndicatorError {
    let needs = match kind {
        Kind::Int => "a whole number",
        Kind::Float => "a number",
        Kind::Str => "text",
        Kind::Bool => "true or false",
    };
    IndicatorError::type_error(format!(
        "{}: {} must be {}, got {}",
        spec.name,
        param,
        needs,
        py_repr(value)
    ))
}
