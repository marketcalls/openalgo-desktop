//! Fixture loading for the golden tests: the records a case's recipe names,
//! rebuilt from the Agent's `candles.csv` exactly as
//! `scripts/gen_flow_indicator_goldens.py` built them, and the comparison.

use chrono::{DateTime, NaiveDateTime};
use serde_json::{json, Map, Value};

use crate::agent::indicators::tests::support::{json_diff, shape};
use crate::clock::ist_offset;
use crate::flow::contract::pyfmt;
use crate::flow::indicator_service::IndicatorError;

pub(crate) const COMPUTE: &str =
    include_str!("../../../../tests/fixtures/flow_indicators/compute.json");
pub(crate) const CATALOGUE: &str =
    include_str!("../../../../tests/fixtures/flow_indicators/catalogue.json");
pub(crate) const PARAMS: &str =
    include_str!("../../../../tests/fixtures/flow_indicators/params.json");
pub(crate) const WINDOW: &str =
    include_str!("../../../../tests/fixtures/flow_indicators/window.json");
pub(crate) const BARS: &str = include_str!("../../../../tests/fixtures/flow_indicators/bars.json");
pub(crate) const EXECUTOR: &str =
    include_str!("../../../../tests/fixtures/flow_indicators/executor.json");
pub(crate) const TYPES: &str =
    include_str!("../../../../tests/fixtures/flow_indicators/types.json");
pub(crate) const EDITOR_LISTS: &str =
    include_str!("../../../../tests/fixtures/flow_indicators/editor.json");

/// Every value matches within one unit in the sixth place, the rounding the
/// web applies (the tolerance the Agent's compute goldens use).
const ABS: f64 = 1e-6;
const REL: f64 = 1e-9;

pub(crate) fn json(text: &str) -> Value {
    serde_json::from_str(text).expect("fixture is JSON")
}

/// Epoch seconds as the recorder writes an ISO timestamp.
fn iso_ist(epoch: i64) -> String {
    DateTime::from_timestamp(epoch, 0)
        .expect("timestamp")
        .with_timezone(&ist_offset())
        .format("%Y-%m-%dT%H:%M:%S+05:30")
        .to_string()
}

/// The records a recipe names: a shape of `candles.csv`, a row range,
/// columns dropped, filled, or written as text, timestamps as ISO text.
pub(crate) fn records(recipe: &Value) -> Vec<Value> {
    let shape = shape(recipe["shape"].as_str().expect("shape"));
    let n = shape.timestamps.len();
    let start = recipe["start"].as_u64().map_or(0, |v| v as usize);
    let end = recipe["end"].as_u64().map_or(n, |v| v as usize).min(n);
    let listed = |key: &str, column: &str| {
        recipe[key]
            .as_array()
            .is_some_and(|a| a.iter().any(|c| c == column))
    };
    let iso = recipe["iso"].as_bool() == Some(true);
    (start..end)
        .map(|i| {
            let stamp = shape.timestamps[i];
            let mut record = Map::new();
            record.insert(
                "timestamp".into(),
                if iso {
                    json!(iso_ist(stamp))
                } else {
                    json!(stamp)
                },
            );
            for column in ["open", "high", "low", "close", "volume"] {
                if listed("drop", column) {
                    continue;
                }
                let raw = shape.columns[column][i];
                let mut value = if raw.is_nan() {
                    Value::Null
                } else if column == "volume" {
                    json!(raw as i64)
                } else {
                    json!(raw)
                };
                if let Some(fill) = recipe["fill"].get(column) {
                    value = fill.clone();
                }
                if listed("text", column) {
                    if let Some(number) = value.as_f64() {
                        value = json!(match value.as_i64() {
                            Some(int) => int.to_string(),
                            None => pyfmt::float(number),
                        });
                    }
                }
                record.insert(column.into(), value);
            }
            Value::Object(record)
        })
        .collect()
}

/// The fixture's ordered `[key, value]` pairs.
pub(crate) fn pairs(value: &Value) -> Vec<(String, Value)> {
    value
        .as_array()
        .expect("pairs")
        .iter()
        .map(|pair| (pair[0].as_str().expect("key").to_string(), pair[1].clone()))
        .collect()
}

/// A number list where the recorder wrote NaN and the infinities as text.
pub(crate) fn numbers(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .expect("numbers")
        .iter()
        .map(|v| match v.as_str() {
            Some("nan") => f64::NAN,
            Some("inf") => f64::INFINITY,
            Some("-inf") => f64::NEG_INFINITY,
            _ => v.as_f64().expect("number"),
        })
        .collect()
}

/// The rows the web rounds to six places (`_at`: `round(float(v), 6)`).
const ROUNDED: [&str; 4] = ["latest", "previous", "at_offset", "series"];

/// The first difference between a result and the web's. The rows the web
/// rounds are compared exactly (IND-048), so a value left unrounded, or
/// rounded otherwise than Python's `round(x, 6)`, differs; everything else
/// within tolerance. The recorder writes a non-finite float as its Python
/// text, which is how the run keeps one (`pyjson::float_value`).
pub(crate) fn diff(got: &Value, want: &Value) -> Option<String> {
    for key in ROUNDED {
        if let (Some(g), Some(w)) = (got.get(key), want.get(key)) {
            if let Some(problem) = json_diff(key, g, w, 0.0, 0.0) {
                return Some(format!("{problem} (rounded to six places: exact)"));
            }
        }
    }
    json_diff("result", got, want, ABS, REL)
}

/// The first difference between an error and the web's exception.
pub(crate) fn error_diff(got: &IndicatorError, want: &Value) -> Option<String> {
    let want = (
        want["kind"].as_str().expect("kind"),
        want["message"].as_str().expect("message"),
    );
    (got.kind != want.0 || got.message != want.1)
        .then(|| format!("{}: {} != {}: {}", got.kind, got.message, want.0, want.1))
}

/// An IST wall time the recorder wrote with `isoformat()`.
pub(crate) fn wall_time(value: &Value) -> NaiveDateTime {
    let text = value.as_str().expect("time");
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M"))
        .expect("isoformat time")
}

/// Collects failures so one run reports all of them.
#[derive(Default)]
pub(crate) struct Failures(Vec<String>);

impl Failures {
    pub(crate) fn push(&mut self, label: impl std::fmt::Display, detail: impl std::fmt::Display) {
        self.0.push(format!("{label}: {detail}"));
    }

    pub(crate) fn check(self, checked: usize) {
        assert!(checked > 0, "no cases checked");
        assert!(
            self.0.is_empty(),
            "{} of {checked} cases differ from the web:\n{}",
            self.0.len(),
            self.0
                .iter()
                .take(40)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
