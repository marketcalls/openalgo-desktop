//! The dispatcher every indicator call goes through, ported from the web's
//! `services/agent/indicators/compute.py`: one indicator over a cleaned
//! frame, with the web's refusals, rounding and summary.
//!
//! The web also refuses an entry it registers broken. On the desktop none is
//! (lead decision, 1.0.4: `vi` and `ulcerindex` compute on the openalgo build
//! the web pins), so that refusal has no counterpart here.

use serde_json::{Map, Value};

use super::coerce::{coerce_params, resolve_param_aliases};
use super::descriptions::describe;
use super::json::Json;
use super::registry::{self, Default, IndicatorSpec, Status};
use super::ta::{self, ParamValue, Params, Series};
use crate::agent::tools::market::candles::{ist_timestamp, CandleFrame};
use crate::agent::tools::numbers::py_round;
use crate::agent::tools::values::py_repr;

/// Decimal places kept on every returned value (`_ROUND_DP`).
const ROUND_DP: usize = 6;

/// A refusal the caller can fix by changing the request; the sentence is
/// the web's.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IndicatorError(pub String);

impl IndicatorError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for IndicatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One returned value: a number rounded to six places, a signal flag, or
/// `None` where the indicator has no value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Cell {
    Null,
    Num(f64),
    Flag(bool),
}

impl Cell {
    pub(crate) fn json(self) -> Json {
        match self {
            Cell::Null => Json::Null,
            Cell::Num(v) => Json::Num(v),
            Cell::Flag(b) => Json::Bool(b),
        }
    }
}

/// An IST index entry as pandas renders it (`2026-02-11T09:15:00+05:30`).
pub(crate) fn ist_stamp(epoch_seconds: i64) -> String {
    match ist_timestamp(&Value::from(epoch_seconds)) {
        Value::String(text) => text,
        other => other.to_string(),
    }
}

/// The second instrument's closes for `beta` and `correlation`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Comparison {
    pub timestamps: Vec<i64>,
    pub closes: Vec<f64>,
}

/// One computed indicator: the web's `compute()` result.
#[derive(Debug, Clone)]
pub(crate) struct Computed {
    pub(crate) indicator: &'static str,
    pub(crate) category: &'static str,
    pub(crate) params_used: Vec<(&'static str, Json)>,
    pub(crate) bars_used: usize,
    pub(crate) defaults_applied: Option<Vec<String>>,
    pub(crate) outputs: Vec<String>,
    pub(crate) values_returned: usize,
    pub(crate) timestamps: Vec<String>,
    /// The last `last_n` values of each output.
    pub(crate) values: Vec<(String, Vec<Cell>)>,
    /// Each output summarised over the whole series.
    pub(crate) summary: Vec<(String, Json)>,
    pub(crate) note: &'static str,
    pub(crate) requested_as: Option<&'static str>,
}

impl Computed {
    /// The result as the web returns it, keys in the web's order.
    pub(crate) fn to_json(&self) -> Json {
        let mut out = Json::obj([
            ("indicator", Json::str(self.indicator)),
            ("category", Json::str(self.category)),
            ("description", Json::str(describe(self.indicator))),
            ("params_used", Json::obj(self.params_used.iter().cloned())),
            ("bars_used", Json::Int(self.bars_used as i64)),
            (
                "defaults_applied",
                match &self.defaults_applied {
                    Some(keys) => Json::Arr(keys.iter().map(Json::str).collect()),
                    None => Json::Null,
                },
            ),
            (
                "outputs",
                Json::Arr(self.outputs.iter().map(Json::str).collect()),
            ),
            ("values_returned", Json::Int(self.values_returned as i64)),
            (
                "timestamps",
                Json::Arr(self.timestamps.iter().map(Json::str).collect()),
            ),
            ("values", values_json(&self.values)),
            ("summary", Json::obj(self.summary.iter().cloned())),
        ]);
        if !self.note.is_empty() {
            out.set("note", Json::str(self.note));
        }
        if let Some(name) = self.requested_as {
            out.set("requested_as", Json::str(name));
        }
        out
    }
}

pub(crate) fn values_json(values: &[(String, Vec<Cell>)]) -> Json {
    Json::obj(values.iter().map(|(label, cells)| {
        (
            label.clone(),
            Json::Arr(cells.iter().map(|c| c.json()).collect()),
        )
    }))
}

/// A coerced parameter as JSON.
fn param_json(value: &ParamValue) -> Json {
    match value {
        ParamValue::Int(v) => Json::Int(*v),
        ParamValue::Float(v) => Json::Num(*v),
        ParamValue::Bool(b) => Json::Bool(*b),
        ParamValue::Str(s) => Json::str(s.clone()),
    }
}

/// A library default as JSON (`None` for a required parameter).
pub(crate) fn default_json(default: Default) -> Json {
    match default {
        Default::Required => Json::Null,
        Default::Int(v) => Json::Int(v),
        Default::Float(v) => Json::Num(v),
        Default::Str(s) => Json::str(s),
        Default::Bool(b) => Json::Bool(b),
    }
}

/// `_series_for`: the declared inputs as frame columns, refusing a column
/// that is absent, empty or zero on every bar (what "this venue does not
/// publish that" looks like, an index's volume above all).
fn series_for<'f>(
    spec: &IndicatorSpec,
    frame: &'f CandleFrame,
) -> Result<Vec<&'f [f64]>, IndicatorError> {
    let mut out = Vec::with_capacity(spec.inputs.len());
    for column in spec.inputs {
        let Some(values) = frame.column(column) else {
            let carried: Vec<&str> = frame.columns.iter().map(|(name, _)| *name).collect();
            return Err(IndicatorError::new(format!(
                "{} needs the {} series but these candles only carry {}.",
                spec.name,
                py_repr(&Value::String((*column).into())),
                carried.join(", ")
            )));
        };
        let all_nan = values.iter().all(|v| v.is_nan());
        if all_nan || values.iter().all(|v| v.is_nan() || *v == 0.0) {
            let carries = if all_nan {
                "is empty"
            } else {
                "is zero on every bar"
            };
            let hint = if *column == "volume" {
                " An index carries no volume, so a volume indicator cannot be computed on one; \
                 run it on the future or on a share instead."
            } else {
                ""
            };
            return Err(IndicatorError::new(format!(
                "{}: the {} series {} for this instrument, so any value computed from it would \
                 come from the absence of data rather than from the market.{}",
                spec.name,
                py_repr(&Value::String((*column).into())),
                carries,
                hint
            )));
        }
        out.push(values);
    }
    Ok(out)
}

/// The comparison closes on this frame's bars: matched by timestamp, then
/// carried forward, then back (`reindex(...).ffill().bfill()`).
fn align_second(frame: &CandleFrame, second: &Comparison) -> Vec<f64> {
    let lookup: std::collections::HashMap<i64, f64> = second
        .timestamps
        .iter()
        .copied()
        .zip(second.closes.iter().copied())
        .collect();
    let mut aligned: Vec<f64> = frame
        .time
        .iter()
        .map(|t| lookup.get(t).copied().unwrap_or(f64::NAN))
        .collect();
    let mut last = f64::NAN;
    for v in aligned.iter_mut() {
        if v.is_nan() {
            *v = last;
        } else {
            last = *v;
        }
    }
    let mut next = f64::NAN;
    for v in aligned.iter_mut().rev() {
        if v.is_nan() {
            *v = next;
        } else {
            next = *v;
        }
    }
    aligned
}

/// `_to_list`: NaN and the infinities to `None`, flags kept, numbers
/// rounded to six places.
fn to_cells(series: &Series) -> Vec<Cell> {
    match series {
        Series::Bool(flags) => flags.iter().map(|b| Cell::Flag(*b)).collect(),
        Series::Float(values) => values
            .iter()
            .map(|v| {
                if v.is_finite() {
                    Cell::Num(py_round(*v, ROUND_DP))
                } else {
                    Cell::Null
                }
            })
            .collect(),
    }
}

/// `_stats`: the latest value, how many are real, the range and the
/// direction; a flag output reports how many bars fired instead.
pub(crate) fn stats(values: &[Cell]) -> Json {
    let flags: Vec<bool> = values
        .iter()
        .filter_map(|c| {
            if let Cell::Flag(b) = c {
                Some(*b)
            } else {
                None
            }
        })
        .collect();
    if let Some(&latest) = flags.last() {
        return Json::obj([
            ("latest", Json::Bool(latest)),
            ("kind", Json::str("signal")),
            (
                "bars_true",
                Json::Int(flags.iter().filter(|b| **b).count() as i64),
            ),
            ("finite_count", Json::Int(flags.len() as i64)),
        ]);
    }
    let finite: Vec<f64> = values
        .iter()
        .filter_map(|c| if let Cell::Num(v) = c { Some(*v) } else { None })
        .collect();
    let Some(&latest) = finite.last() else {
        return Json::obj([("latest", Json::Null), ("finite_count", Json::Int(0))]);
    };
    let min = finite
        .iter()
        .copied()
        .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
    let max = finite
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, |a, b| if b > a { b } else { a });
    let mut out = Json::obj([
        ("latest", Json::Num(latest)),
        ("finite_count", Json::Int(finite.len() as i64)),
        ("min", Json::Num(min)),
        ("max", Json::Num(max)),
    ]);
    if finite.len() >= 2 {
        let previous = finite[finite.len() - 2];
        let direction = if latest > previous {
            "rising"
        } else if latest < previous {
            "falling"
        } else {
            "flat"
        };
        out.set("previous", Json::Num(previous));
        out.set("direction", Json::str(direction));
    }
    out
}

/// `_resolve_outputs`: the alternate names when an argument value changes
/// how many series come back.
fn resolve_outputs(spec: &IndicatorSpec, params: &[(&'static str, ParamValue)]) -> Vec<String> {
    for (condition, outputs) in spec.conditional_outputs {
        let (key, raw) = condition.split_once('=').unwrap_or((condition, ""));
        if let Some((_, value)) = params.iter().find(|(k, _)| *k == key) {
            if value.display() == raw {
                return outputs.iter().map(|s| s.to_string()).collect();
            }
        }
    }
    spec.outputs.iter().map(|s| s.to_string()).collect()
}

/// The library's own failure as the web words it.
fn library_error(name: &str, error: ta::TaError) -> IndicatorError {
    IndicatorError::new(match error.kind {
        "TypeError" => format!("{}: bad arguments, {}", name, error.message),
        "ValueError" => format!("{}: {}", name, error.message),
        other => format!(
            "{} could not be computed on these candles: {}, {}",
            name, other, error.message
        ),
    })
}

/// The method's parameters: the coerced ones, then its defaults.
pub(crate) fn bind(spec: &'static IndicatorSpec, clean: &[(&'static str, ParamValue)]) -> Params {
    let values = spec
        .params
        .iter()
        .filter_map(|p| {
            if let Some((_, v)) = clean.iter().find(|(k, _)| *k == p.name) {
                return Some((p.name, v.clone()));
            }
            Some((
                p.name,
                match p.default {
                    Default::Required => return None,
                    Default::Int(v) => ParamValue::Int(v),
                    Default::Float(v) => ParamValue::Float(v),
                    Default::Str(s) => ParamValue::Str(s.into()),
                    Default::Bool(b) => ParamValue::Bool(b),
                },
            ))
        })
        .collect();
    Params::new(spec.name, values)
}

/// Run one `ta` method the way the web's dispatcher calls it.
pub(crate) fn call_library(
    spec: &'static IndicatorSpec,
    series: &[&[f64]],
    clean: &[(&'static str, ParamValue)],
) -> Result<Vec<Series>, IndicatorError> {
    let params = bind(spec, clean);
    match ta::call(spec.name, series, &params) {
        Some(Ok(parts)) => Ok(parts),
        Some(Err(error)) => Err(library_error(spec.name, error)),
        None => Err(IndicatorError::new(format!(
            "{} could not be computed on these candles: AttributeError, \
             'TechnicalAnalysis' object has no attribute '{}'",
            spec.name, spec.name
        ))),
    }
}

/// `compute`: one indicator over a cleaned frame. `name` is the exact `ta`
/// method name, already lower-cased; `second` is the comparison instrument
/// for `beta` and `correlation`; `last_n` values of each output come back.
pub(crate) fn compute(
    name: &str,
    frame: &CandleFrame,
    params: Option<&Map<String, Value>>,
    second: Option<&Comparison>,
    last_n: i64,
) -> Result<Computed, IndicatorError> {
    let Some(spec) = registry::get_spec(name) else {
        return Err(IndicatorError::new(format!(
            "Unknown indicator {}. Call list_indicators to see the {} available names.",
            py_repr(&Value::String(name.into())),
            registry::count()
        )));
    };
    if spec.inputs.is_empty() && !spec.needs_second_series {
        return Err(IndicatorError::new(format!(
            "{} cannot be computed directly. {}",
            spec.name, spec.note
        )));
    }
    let target = match (spec.status, spec.alias_of) {
        (Status::Alias, Some(alias)) => registry::get_spec(alias).unwrap_or(spec),
        _ => spec,
    };
    let clean = coerce_params(target, params)?;
    if frame.is_empty() {
        return Err(IndicatorError::new(format!(
            "{}: no candles came back for this request. Check the symbol, the exchange and the \
             interval, and that the range covers a trading day.",
            target.name
        )));
    }
    let parts = run(target, frame, &clean, second)?;
    Ok(shape(spec, target, frame, params, clean, parts, last_n))
}

/// The checks that need the frame, then the library call.
fn run(
    target: &'static IndicatorSpec,
    frame: &CandleFrame,
    clean: &[(&'static str, ParamValue)],
    second: Option<&Comparison>,
) -> Result<Vec<Series>, IndicatorError> {
    let aligned: Vec<f64>;
    let mut series = series_for(target, frame)?;
    if target.needs_second_series {
        let Some(second) = second else {
            return Err(IndicatorError::new(format!(
                "{} compares two instruments and needs a second one. Supply compare_symbol and \
                 compare_exchange.",
                target.name
            )));
        };
        aligned = align_second(frame, second);
        if aligned.iter().all(|v| v.is_nan()) {
            return Err(IndicatorError::new(format!(
                "{}: the comparison instrument's candles do not overlap this one's. Use the same \
                 interval and a range both instruments traded in.",
                target.name
            )));
        }
        series.truncate(1);
        series.push(&aligned);
    }
    let longest = clean
        .iter()
        .filter_map(|(key, value)| match (target.param(key), value) {
            (Some(p), ParamValue::Int(v)) if p.kind == registry::Kind::Int => Some(*v),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    if longest != 0 && frame.len() as i64 <= longest {
        return Err(IndicatorError::new(format!(
            "{}: a period of {} needs more than the {} bars available. Raise lookback_bars, use a \
             larger interval, or use a shorter period.",
            target.name,
            longest,
            frame.len()
        )));
    }
    call_library(target, &series, clean)
}

/// The result dict: tails, summaries, the parameters used.
fn shape(
    spec: &'static IndicatorSpec,
    target: &'static IndicatorSpec,
    frame: &CandleFrame,
    params: Option<&Map<String, Value>>,
    clean: Vec<(&'static str, ParamValue)>,
    parts: Vec<Series>,
    last_n: i64,
) -> Computed {
    let mut outputs = resolve_outputs(target, &clean);
    if parts.len() != outputs.len() {
        outputs = (1..=parts.len()).map(|i| format!("output_{}", i)).collect();
    }
    let tail = if last_n == 0 {
        10
    } else {
        last_n.max(1) as usize
    };
    let mut values = Vec::with_capacity(parts.len());
    let mut summary = Vec::with_capacity(parts.len());
    for (label, part) in outputs.iter().zip(&parts) {
        let full = to_cells(part);
        summary.push((label.clone(), stats(&full)));
        let start = full.len().saturating_sub(tail);
        values.push((label.clone(), full[start..].to_vec()));
    }
    let given: Vec<String> = resolve_param_aliases(target, params)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let mut applied: Vec<String> = clean
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !given.contains(k))
        .collect();
    applied.sort();
    let params_used = target
        .params
        .iter()
        .map(|p| match clean.iter().find(|(k, _)| *k == p.name) {
            Some((_, v)) => (p.name, param_json(v)),
            None => (p.name, default_json(p.default)),
        })
        .collect();
    let start = frame.len().saturating_sub(tail);
    Computed {
        indicator: target.name,
        category: target.category,
        params_used,
        bars_used: frame.len(),
        defaults_applied: (!applied.is_empty()).then_some(applied),
        outputs,
        values_returned: tail.min(frame.len()),
        timestamps: frame.time[start..].iter().map(|t| ist_stamp(*t)).collect(),
        values,
        summary,
        note: target.note,
        requested_as: (spec.name != target.name).then_some(spec.name),
    }
}
