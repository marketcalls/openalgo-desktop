//! Every `ta` method against the web's installed openalgo 2.0.5, on every
//! candle shape, with its defaults and with the parameter variants the
//! recorder chose (`raw_<shape>.json`).
//!
//! Tolerance, per output: a value matches when it is within
//! `1e-9 * |expected| + 1e-9 * scale`, where `scale` is the largest absolute
//! value of that output. The fixtures keep 10 significant digits, so the
//! first term absorbs their rounding; the second absorbs the last-bit
//! differences of a value that is a small difference of large ones (a MACD
//! histogram near zero, an OBV step), where `libm` and NumPy's SIMD `log`
//! and `sqrt` may disagree in the last bit. NaN placement, booleans, the
//! output count and every error's type and sentence must match exactly.

use serde_json::Value;

use super::support::{close, json, param_value, shape, RAW};
use crate::agent::indicators::compute::bind;
use crate::agent::indicators::registry::get_spec;
use crate::agent::indicators::ta::{self, ParamValue, Series};

const REL: f64 = 1e-9;

/// Compare one output with its golden values; the first mismatch, if any.
fn compare_output(got: &Series, want: &[Value]) -> Option<String> {
    let scale = want
        .iter()
        .filter_map(Value::as_f64)
        .fold(0.0f64, |m, v| m.max(v.abs()));
    match got {
        Series::Bool(flags) => {
            if flags.len() != want.len() {
                return Some(format!("{} flags, want {}", flags.len(), want.len()));
            }
            flags.iter().zip(want).enumerate().find_map(|(i, (g, w))| {
                (w.as_bool() != Some(*g)).then(|| format!("bar {i}: {g} != {w}"))
            })
        }
        Series::Float(values) => {
            if values.len() != want.len() {
                return Some(format!("{} values, want {}", values.len(), want.len()));
            }
            values.iter().zip(want).enumerate().find_map(|(i, (g, w))| {
                let ok = match w {
                    Value::Null => g.is_nan(),
                    Value::String(s) if s == "inf" => *g == f64::INFINITY,
                    Value::String(s) if s == "-inf" => *g == f64::NEG_INFINITY,
                    Value::Number(n) => {
                        let w = n.as_f64().unwrap_or(f64::NAN);
                        close(*g, w, REL * scale, REL)
                    }
                    _ => false,
                };
                (!ok).then(|| format!("bar {i}: {g} != {w}"))
            })
        }
    }
}

/// Run one recorded case; the failure, if any.
fn run_case(shape_name: &str, candles: &super::support::Shape, case: &Value) -> Option<String> {
    let name = case["name"].as_str()?;
    let spec = get_spec(name)?;
    let inputs: Vec<&[f64]> = case["inputs"]
        .as_array()?
        .iter()
        .map(|c| candles.columns[c.as_str().unwrap_or_default()].as_slice())
        .collect();
    let clean: Vec<(&'static str, ParamValue)> = case["params"]
        .as_object()?
        .iter()
        .map(|(k, v)| (spec.param(k).map_or("?", |p| p.name), param_value(v)))
        .collect();
    let label = format!("{shape_name} {name} {}", case["params"]);
    let outcome = match ta::call(name, &inputs, &bind(spec, &clean)) {
        Some(outcome) => outcome,
        None => return Some(format!("{label}: not dispatched")),
    };
    match (outcome, case.get("error")) {
        (Err(e), Some(want)) => {
            let same = want["kind"] == e.kind && want["message"] == e.message.as_str();
            (!same).then(|| format!("{label}: {}: {} != {want}", e.kind, e.message))
        }
        (Err(e), None) => Some(format!("{label}: raised {}: {}", e.kind, e.message)),
        (Ok(_), Some(want)) => Some(format!("{label}: computed, want {want}")),
        (Ok(parts), None) => {
            let want = case["outputs"].as_array()?;
            if parts.len() != want.len() {
                return Some(format!(
                    "{label}: {} outputs, want {}",
                    parts.len(),
                    want.len()
                ));
            }
            parts.iter().zip(want).enumerate().find_map(|(i, (g, w))| {
                compare_output(g, w.as_array()?).map(|d| format!("{label} output {i}: {d}"))
            })
        }
    }
}

#[test]
fn every_method_matches_the_web_on_every_shape() {
    let mut failures = Vec::new();
    let mut cases = 0;
    let mut covered = std::collections::BTreeSet::new();
    for (shape_name, text) in RAW {
        let fixture = json(text);
        let candles = shape(shape_name);
        for case in fixture["cases"].as_array().expect("cases") {
            cases += 1;
            covered.insert(case["name"].as_str().unwrap_or_default().to_string());
            if let Some(failure) = run_case(shape_name, &candles, case) {
                failures.push(failure);
            }
        }
    }
    assert_eq!(covered.len(), 127, "every registry entry has golden cases");
    assert!(cases > 900, "only {cases} golden cases");
    assert!(
        failures.is_empty(),
        "{} of {cases} cases differ from the web:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
