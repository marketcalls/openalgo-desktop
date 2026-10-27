//! Fixture loading for the golden tests: the candles the web's package ran
//! over, the frames `compute.json` names, and the tolerance comparison.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::agent::indicators::compute::Comparison;
use crate::agent::indicators::frame::CandleFrame;
use crate::agent::indicators::ta::ParamValue;

pub(super) const CANDLES: &str =
    include_str!("../../../../tests/fixtures/agent_indicators/candles.csv");
pub(super) const REGISTRY: &str =
    include_str!("../../../../tests/fixtures/agent_indicators/registry.json");
pub(super) const COMPUTE: &str =
    include_str!("../../../../tests/fixtures/agent_indicators/compute.json");

/// The raw golden files, one per candle shape.
pub(super) const RAW: [(&str, &str); 5] = [
    (
        "trend",
        include_str!("../../../../tests/fixtures/agent_indicators/raw_trend.json"),
    ),
    (
        "range",
        include_str!("../../../../tests/fixtures/agent_indicators/raw_range.json"),
    ),
    (
        "gaps",
        include_str!("../../../../tests/fixtures/agent_indicators/raw_gaps.json"),
    ),
    (
        "nan_head",
        include_str!("../../../../tests/fixtures/agent_indicators/raw_nan_head.json"),
    ),
    (
        "tiny",
        include_str!("../../../../tests/fixtures/agent_indicators/raw_tiny.json"),
    ),
];

pub(super) fn json(text: &str) -> Value {
    serde_json::from_str(text).expect("fixture is JSON")
}

/// One shape of candles.csv: timestamps and every column by name.
pub(super) struct Shape {
    pub timestamps: Vec<i64>,
    pub columns: BTreeMap<String, Vec<f64>>,
}

pub(super) fn shape(name: &str) -> Shape {
    let mut lines = CANDLES.lines();
    let header: Vec<&str> = lines.next().expect("header").split(',').collect();
    let mut out = Shape {
        timestamps: Vec::new(),
        columns: BTreeMap::new(),
    };
    for line in lines {
        let cells: Vec<&str> = line.split(',').collect();
        if cells[0] != name {
            continue;
        }
        out.timestamps.push(cells[1].parse().expect("timestamp"));
        for (column, cell) in header.iter().zip(&cells).skip(2) {
            let value = if cell.is_empty() {
                f64::NAN
            } else {
                cell.parse().expect("number")
            };
            out.columns
                .entry(column.to_string())
                .or_default()
                .push(value);
        }
    }
    assert!(!out.timestamps.is_empty(), "no candles for shape {name}");
    out
}

const OHLCV: [&str; 5] = ["open", "high", "low", "close", "volume"];

/// A frame from `compute.json`'s `frames` recipe: a shape, a row range,
/// columns dropped, columns replaced by one value.
pub(super) fn frame(recipe: &Value) -> CandleFrame {
    let shape = shape(recipe["shape"].as_str().expect("shape"));
    let n = shape.timestamps.len();
    let start = recipe["start"].as_u64().map_or(0, |v| v as usize);
    let end = recipe["end"].as_u64().map_or(n, |v| v as usize).min(n);
    let dropped: Vec<&str> = recipe["drop"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let columns = OHLCV
        .iter()
        .filter(|c| !dropped.contains(c))
        .map(|c| {
            let mut values = shape.columns[*c][start..end].to_vec();
            if let Some(fill) = recipe["fill"].get(*c) {
                let value = match fill {
                    Value::String(s) if s == "nan" => f64::NAN,
                    other => other.as_f64().expect("fill value"),
                };
                values.iter_mut().for_each(|v| *v = value);
            }
            (c.to_string(), values)
        })
        .collect();
    CandleFrame::new(shape.timestamps[start..end].to_vec(), columns)
}

/// The comparison closes from `compute.json`'s `seconds` recipe.
pub(super) fn second(recipe: &Value) -> Comparison {
    let shape = shape(recipe["shape"].as_str().expect("shape"));
    let mut stamps = shape.timestamps.clone();
    let mut closes = shape.columns["bench"].clone();
    if recipe["gappy"].as_bool() == Some(true) {
        let keep: Vec<usize> = (0..stamps.len()).filter(|i| i % 7 != 3).collect();
        let extra_stamp = stamps[stamps.len() - 1] + 86_400 * 3;
        let extra_close = closes[closes.len() - 1];
        stamps = keep
            .iter()
            .map(|&i| stamps[i])
            .chain([extra_stamp])
            .collect();
        closes = keep
            .iter()
            .map(|&i| closes[i])
            .chain([extra_close])
            .collect();
    }
    let shift = recipe["shift_days"].as_i64().unwrap_or(0) * 86_400;
    let start = recipe["start"].as_u64().map_or(0, |v| v as usize);
    let end = recipe["end"]
        .as_u64()
        .map_or(stamps.len(), |v| v as usize)
        .min(stamps.len());
    Comparison {
        timestamps: stamps[start..end].iter().map(|t| t + shift).collect(),
        closes: closes[start..end].to_vec(),
    }
}

/// A fixture parameter as the library receives it.
pub(super) fn param_value(value: &Value) -> ParamValue {
    match value {
        Value::Bool(b) => ParamValue::Bool(*b),
        Value::Number(n) if n.is_i64() => ParamValue::Int(n.as_i64().unwrap_or_default()),
        Value::Number(n) => ParamValue::Float(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => ParamValue::Str(s.clone()),
        other => panic!("unexpected fixture parameter {other}"),
    }
}

/// `|got - want| <= abs + rel * |want|`.
pub(super) fn close(got: f64, want: f64, abs: f64, rel: f64) -> bool {
    (got - want).abs() <= abs + rel * want.abs()
}

/// Compare two JSON values: structure and text exactly, numbers within
/// `abs + rel * |want|`. Returns the first difference.
pub(super) fn json_diff(
    path: &str,
    got: &Value,
    want: &Value,
    abs: f64,
    rel: f64,
) -> Option<String> {
    match (got, want) {
        (Value::Number(g), Value::Number(w)) => {
            let (g, w) = (g.as_f64()?, w.as_f64()?);
            (!close(g, w, abs, rel)).then(|| format!("{path}: {g} != {w}"))
        }
        (Value::Array(g), Value::Array(w)) => {
            if g.len() != w.len() {
                return Some(format!("{path}: {} items != {}", g.len(), w.len()));
            }
            g.iter()
                .zip(w)
                .enumerate()
                .find_map(|(i, (a, b))| json_diff(&format!("{path}[{i}]"), a, b, abs, rel))
        }
        (Value::Object(g), Value::Object(w)) => {
            let gk: Vec<&String> = g.keys().collect();
            let wk: Vec<&String> = w.keys().collect();
            if gk != wk {
                return Some(format!("{path}: keys {gk:?} != {wk:?}"));
            }
            g.iter()
                .find_map(|(k, v)| json_diff(&format!("{path}.{k}"), v, &w[k], abs, rel))
        }
        _ => (got != want).then(|| format!("{path}: {got} != {want}")),
    }
}
