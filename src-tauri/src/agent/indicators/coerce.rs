//! Argument handling for one indicator call, ported from the web's
//! `services/agent/indicators/compute.py`: borrowed parameter names,
//! coercion of JSON numbers to what the library accepts, the refusals, and
//! the window sizing that pads a fetch by the indicator's warm-up.

use serde_json::{Map, Value};

use super::compute::IndicatorError;
use super::pyfmt::{py_repr, py_str};
use super::registry::{required_default, IndicatorSpec, Kind};
use super::ta::ParamValue;

/// Extra bars fetched on top of the measured warm-up (`WARMUP_MARGIN`).
pub(crate) const WARMUP_MARGIN: usize = 30;

/// Floor on the bars any request fetches (`MIN_BARS`).
pub(crate) const MIN_BARS: usize = 60;

/// Names a model borrows from other charting platforms and the real names
/// each may stand for (`_PARAM_ALIASES`).
const PARAM_ALIASES: &[(&str, &[&str])] = &[
    ("length", &["period"]),
    ("fast", &["fast_period", "fast_length"]),
    ("slow", &["slow_period", "slow_length"]),
    ("signal", &["signal_period", "signal_length"]),
    ("std", &["std_dev"]),
    ("stdev", &["std_dev"]),
    ("stddev", &["std_dev"]),
    ("deviation", &["std_dev"]),
];

/// `resolve_param_aliases`: keys lower-cased and trimmed, and a borrowed
/// name renamed only when exactly one of its candidates is a parameter of
/// this indicator and the real name was not also given.
pub(crate) fn resolve_param_aliases(
    spec: &IndicatorSpec,
    params: Option<&Map<String, Value>>,
) -> Vec<(String, Value)> {
    let mut lowered: Vec<(String, Value)> = Vec::new();
    for (key, value) in params.into_iter().flatten() {
        let key = key.trim().to_lowercase();
        match lowered.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value.clone(),
            None => lowered.push((key, value.clone())),
        }
    }
    let given: Vec<String> = lowered.iter().map(|(k, _)| k.clone()).collect();
    let mut out: Vec<(String, Value)> = Vec::new();
    for (key, value) in lowered {
        let mut key = key;
        if spec.param(&key).is_none() {
            let candidates = PARAM_ALIASES
                .iter()
                .find(|(alias, _)| *alias == key)
                .map(|(_, names)| *names)
                .unwrap_or(&[]);
            let matches: Vec<&str> = candidates
                .iter()
                .copied()
                .filter(|name| spec.param(name).is_some())
                .collect();
            if matches.len() == 1 && !given.iter().any(|g| g == matches[0]) {
                key = matches[0].to_string();
            }
        }
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => out.push((key, value)),
        }
    }
    out
}

/// Python `float(value)` for a JSON-decoded value, or None where Python
/// raises.
pub(crate) fn py_float(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => parse_float_text(s),
        _ => None,
    }
}

/// `float(str)`: surrounding whitespace, single underscores between digits,
/// `inf`, `infinity` and `nan` in any case.
fn parse_float_text(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    let mut cleaned = String::with_capacity(trimmed.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let between = i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
            if !between {
                return None;
            }
            continue;
        }
        cleaned.push(c);
    }
    let unsigned = cleaned.trim_start_matches(['+', '-']);
    let word = unsigned.to_ascii_lowercase();
    if word == "inf" || word == "infinity" || word == "nan" {
        let negative = cleaned.starts_with('-');
        let magnitude = if word == "nan" {
            f64::NAN
        } else {
            f64::INFINITY
        };
        return Some(if negative { -magnitude } else { magnitude });
    }
    if unsigned
        .chars()
        .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
    {
        return None;
    }
    cleaned.parse().ok()
}

/// Python truthiness of a JSON-decoded value (`bool(value)`).
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// A whole float as Python's `int()` prints it, beyond `i64` too (and `0`,
/// never `-0`).
fn whole_text(value: f64) -> String {
    format!("{:.0}", value + 0.0)
}

/// One parameter value, coerced to its kind (`_coerce_params`' loop body).
fn coerce_one(
    spec: &IndicatorSpec,
    key: &str,
    kind: Kind,
    value: &Value,
) -> Result<ParamValue, IndicatorError> {
    let name = spec.name;
    Ok(match kind {
        Kind::Int => {
            let whole = py_float(value).filter(|x| x.is_finite() && x.fract() == 0.0);
            let Some(as_float) = whole else {
                return Err(IndicatorError::new(format!(
                    "{}.{} must be a whole number, got {}.",
                    name,
                    key,
                    py_repr(value)
                )));
            };
            if as_float <= 0.0 {
                return Err(IndicatorError::new(format!(
                    "{}.{} must be positive, got {}.",
                    name,
                    key,
                    whole_text(as_float)
                )));
            }
            ParamValue::Int(as_float.min(i64::MAX as f64) as i64)
        }
        Kind::Float => match py_float(value) {
            Some(number) => ParamValue::Float(number),
            None => {
                return Err(IndicatorError::new(format!(
                    "{}.{} must be a number, got {}.",
                    name,
                    key,
                    py_repr(value)
                )))
            }
        },
        Kind::Bool => ParamValue::Bool(match value {
            Value::String(s) => matches!(s.trim().to_lowercase().as_str(), "1" | "true" | "yes"),
            other => truthy(other),
        }),
        Kind::Str => {
            let text = py_str(value);
            let allowed = spec.param(key).map(|p| p.allowed).unwrap_or(&[]);
            if !allowed.is_empty() && !allowed.contains(&text.as_str()) {
                let listed: Vec<String> = allowed.iter().map(|a| format!("'{}'", a)).collect();
                return Err(IndicatorError::new(format!(
                    "{}.{} must be one of [{}], got {}.",
                    name,
                    key,
                    listed.join(", "),
                    py_repr(&Value::String(text))
                )));
            }
            ParamValue::Str(text)
        }
    })
}

/// `_coerce_params`: the caller's parameters as the library accepts them,
/// plus the shared default for a required one left out.
pub(crate) fn coerce_params(
    spec: &IndicatorSpec,
    params: Option<&Map<String, Value>>,
) -> Result<Vec<(&'static str, ParamValue)>, IndicatorError> {
    let mut out: Vec<(&'static str, ParamValue)> = Vec::new();
    for (key, value) in resolve_param_aliases(spec, params) {
        let Some(pspec) = spec.param(&key) else {
            let valid: Vec<&str> = spec.params.iter().map(|p| p.name).collect();
            return Err(IndicatorError::new(format!(
                "{} has no parameter {}. Valid parameters: {}.",
                spec.name,
                py_repr(&Value::String(key)),
                if valid.is_empty() {
                    "none".to_string()
                } else {
                    valid.join(", ")
                }
            )));
        };
        if value.is_null() {
            continue;
        }
        let coerced = coerce_one(spec, &key, pspec.kind, &value)?;
        out.push((pspec.name, coerced));
    }
    if let Some((key, value)) = required_default(spec.name) {
        if spec.param(key).is_some() && !out.iter().any(|(k, _)| *k == key) {
            out.push((key, ParamValue::Int(value)));
        }
    }
    Ok(out)
}

/// `required_bars`: how many bars to fetch so `want` finite values come
/// back. The measured warm-up, raised by a period longer than the default,
/// plus a margin, never below 60.
pub fn required_bars(
    spec: &IndicatorSpec,
    params: Option<&Map<String, Value>>,
    want: usize,
) -> usize {
    let mut warmup = spec.warmup as i64;
    for (key, value) in resolve_param_aliases(spec, params) {
        let Some(pspec) = spec.param(&key) else {
            continue;
        };
        if pspec.kind != Kind::Int {
            continue;
        }
        let Some(requested) = py_float(&value).filter(|x| x.is_finite()) else {
            continue;
        };
        let requested = requested.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64;
        match pspec.default {
            super::registry::Default::Int(default) => {
                if requested > default {
                    warmup = warmup.saturating_add(requested - default);
                }
            }
            _ => warmup = warmup.max(requested),
        }
    }
    let total = warmup.saturating_add((want + WARMUP_MARGIN) as i64);
    usize::try_from(total).unwrap_or(0).max(MIN_BARS)
}
