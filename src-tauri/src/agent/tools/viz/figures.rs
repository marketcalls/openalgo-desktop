//! Plotly figures for the `plotly` viz kind (the web's `plotly_spec`,
//! `_markers`, `_vertical` and `_peak` in `tools/viz.py`).
//!
//! The only colours chosen here carry meaning: calls green and puts red
//! (matching the `/tools` option analytics), a positive bar green and a
//! negative one red. Fonts, paper and every other colour belong to the
//! client's theme, which merges itself under the spec.

use crate::agent::contract::json::json_safe;
use crate::agent::tools::numbers::as_number;
use serde_json::{json, Map, Value};

/// A call, wherever the conversation draws one.
pub const CALL_COLOUR: &str = "#22c55e";
/// A put, wherever the conversation draws one.
pub const PUT_COLOUR: &str = "#ef4444";
/// A bar at or above zero.
pub const POSITIVE_COLOUR: &str = "#22c55e";
/// A bar below zero.
pub const NEGATIVE_COLOUR: &str = "#ef4444";

const ATM_COLOUR: &str = "#94a3b8";
const SPOT_COLOUR: &str = "#0ea5e9";

/// The figure envelope: the engine (`2d` for `Plot2D`, `3d` for `Plot3D`),
/// the traces, the layout over the shared margins, and a config without a
/// mode bar (the chart sits in a conversation).
pub fn plotly_spec(
    data: Vec<Value>,
    layout: Map<String, Value>,
    engine: &str,
) -> Map<String, Value> {
    let mut merged = Map::new();
    merged.insert("autosize".into(), json!(true));
    merged.insert("margin".into(), json!({"l": 56, "r": 24, "t": 32, "b": 48}));
    for (k, v) in layout {
        merged.insert(k, v);
    }
    let mut spec = Map::new();
    spec.insert("engine".into(), json!(engine));
    spec.insert("data".into(), json_safe(&Value::Array(data)));
    spec.insert("layout".into(), json_safe(&Value::Object(merged)));
    spec.insert(
        "config".into(),
        json!({"displayModeBar": false, "responsive": true}),
    );
    spec
}

/// The dotted line and its label marking one price on a strike axis; one
/// label per row above the plot, so ATM and spot never overlap.
fn vertical(x: &Value, colour: &str, label: &str, row: usize) -> (Value, Value) {
    let shape = json!({
        "type": "line",
        "x0": x,
        "x1": x,
        "yref": "paper",
        "y0": 0,
        "y1": 1,
        "line": {"color": colour, "width": 1, "dash": "dot"},
    });
    let annotation = json!({
        "x": x,
        "yref": "paper",
        "y": 1.02 + 0.06 * row as f64,
        "text": label,
        "showarrow": false,
        "font": {"size": 11, "color": colour},
    });
    (shape, annotation)
}

/// The ATM and spot markers of a strike-axis chart, as the layout's
/// `shapes` and `annotations`. A value the service did not supply is
/// skipped, never drawn at zero.
pub fn markers(atm: &Value, spot: &Value) -> (Vec<Value>, Vec<Value>) {
    let mut shapes = Vec::new();
    let mut annotations = Vec::new();
    for (value, colour, label) in [(atm, ATM_COLOUR, "ATM"), (spot, SPOT_COLOUR, "Spot")] {
        if as_number(value).is_none() {
            continue;
        }
        let (shape, annotation) = vertical(value, colour, label, annotations.len());
        shapes.push(shape);
        annotations.push(annotation);
    }
    (shapes, annotations)
}

/// The strike carrying the largest positive value, or null.
pub fn peak(strikes: &[Value], values: &[Option<f64>]) -> Value {
    let mut best = Value::Null;
    let mut best_value = 0.0;
    for (strike, value) in strikes.iter().zip(values) {
        if let Some(v) = value {
            if *v > best_value {
                best_value = *v;
                best = strike.clone();
            }
        }
    }
    best
}

/// A series of optional numbers as JSON (a gap is `null`).
pub fn numbers(values: &[Option<f64>]) -> Value {
    Value::Array(values.iter().map(|v| json!(v)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-231: the envelope, the layout merged over the shared margins.
    #[test]
    fn the_envelope_carries_no_theme_and_no_mode_bar() {
        let mut layout = Map::new();
        layout.insert("barmode".into(), json!("group"));
        layout.insert("margin".into(), json!({"l": 0}));
        let spec = plotly_spec(vec![json!({"type": "bar"})], layout, "2d");
        assert_eq!(
            Value::Object(spec),
            json!({
                "engine": "2d",
                "data": [{"type": "bar"}],
                "layout": {"autosize": true, "margin": {"l": 0}, "barmode": "group"},
                "config": {"displayModeBar": false, "responsive": true},
            })
        );
    }

    /// T-233: ATM and spot stack one label per row; a missing one is not
    /// drawn at zero.
    #[test]
    fn markers_skip_what_the_service_did_not_supply() {
        let (shapes, notes) = markers(&json!(23850), &json!(23873.45));
        assert_eq!(shapes.len(), 2);
        assert_eq!(notes[0]["text"], "ATM");
        assert_eq!(notes[0]["y"], 1.02);
        assert_eq!(notes[1]["text"], "Spot");
        assert_eq!(notes[1]["y"], 1.08);
        assert_eq!(
            shapes[1]["line"],
            json!({"color": "#0ea5e9", "width": 1, "dash": "dot"})
        );
        let (shapes, notes) = markers(&Value::Null, &json!(23873.45));
        assert_eq!(shapes.len(), 1);
        assert_eq!(notes[0]["text"], "Spot");
        assert_eq!(notes[0]["y"], 1.02);
    }

    #[test]
    fn the_peak_is_the_largest_positive_value() {
        let strikes = [json!(100), json!(110), json!(120)];
        assert_eq!(
            peak(&strikes, &[Some(1.0), Some(5.0), Some(5.0)]),
            json!(110)
        );
        assert_eq!(peak(&strikes, &[None, Some(-1.0), Some(0.0)]), Value::Null);
    }
}
