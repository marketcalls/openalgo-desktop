//! The API's own contract, independent of recorded values.

use serde_json::{json, Value};

use crate::flow::indicator_service::{
    clamp_bars, compute_indicator, list_supported_indicators, IndicatorOutput, IndicatorParams,
    IndicatorRequest, IndicatorSource,
};

fn closes(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            let close = 100.0 + (i as f64 * 0.7).sin() * 5.0 + i as f64 * 0.1;
            json!({"timestamp": 1_735_669_800 + i as i64 * 86_400, "open": close, "high": close + 1.0,
                   "low": close - 1.0, "close": close, "volume": 1000 + i})
        })
        .collect()
}

fn run(name: &str, params: &IndicatorParams, source: IndicatorSource<'_>) -> IndicatorOutput {
    compute_indicator(&IndicatorRequest {
        indicator_name: name,
        params,
        source,
        tail_bars: 5,
        offset_bars: 0,
    })
    .expect("computes")
}

#[test]
fn the_catalogue_offers_what_the_web_offers() {
    let names = list_supported_indicators();
    assert_eq!(names.len(), 116);
    assert!(names.windows(2).all(|w| w[0] < w[1]), "sorted");
    assert!(!names.contains(&"crossover") && !names.contains(&"vi"));
}

#[test]
fn bar_counts_are_held_to_the_ceiling() {
    assert_eq!(clamp_bars(500, "bars"), 200);
    assert_eq!(clamp_bars(0, "bars"), 1);
    assert_eq!(clamp_bars(-7, "bars"), 1);
    assert_eq!(clamp_bars(37, "bars"), 37);
}

#[test]
fn params_keep_the_order_written() {
    let params =
        IndicatorParams::parse(r#"{"slow_period": 30, "fast_period": 8, "fast_period": 9}"#)
            .expect("parses");
    let keys: Vec<&str> = params.pairs().iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["slow_period", "fast_period"]);
    assert_eq!(params.pairs()[1].1, json!(9));
    assert_eq!(
        IndicatorParams::parse("  ").expect("empty"),
        IndicatorParams::default()
    );
    assert_eq!(
        IndicatorParams::parse("{period: 14}").unwrap_err(),
        "Invalid params JSON: {period: 14}"
    );
    assert_eq!(
        IndicatorParams::parse("[14]").unwrap_err(),
        "params must resolve to a JSON object, got list"
    );
}

/// The params text is read as Python's `json.loads` reads it: `NaN` and
/// `Infinity` are floats (kept as their Python text, JSON holding no such
/// value), a quoted `"NaN"` stays text, and the keys keep their written
/// order, a repeated key its first place.
#[test]
fn params_read_nan_and_infinity_as_python_does() {
    let params = IndicatorParams::parse(
        r#"{"b": NaN, "a": "NaN", "c": -Infinity, "d": 1e999, "b": Infinity}"#,
    )
    .expect("parses");
    let keys: Vec<&str> = params.pairs().iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["b", "a", "c", "d"]);
    assert_eq!(params.non_finite("b"), Some(f64::INFINITY));
    assert_eq!(params.non_finite("a"), None);
    assert_eq!(params.non_finite("c"), Some(f64::NEG_INFINITY));
    assert_eq!(params.non_finite("d"), Some(f64::INFINITY));
    assert!(params.non_finite("e").is_none());
    let nan = IndicatorParams::parse(r#"{"period": NaN}"#).expect("parses");
    assert!(nan.non_finite("period").is_some_and(f64::is_nan));
    assert_eq!(
        IndicatorParams::parse(" NaN ").unwrap_err(),
        "params must resolve to a JSON object, got float"
    );
    assert_eq!(
        IndicatorParams::parse(r#""NaN""#).unwrap_err(),
        "params must resolve to a JSON object, got str"
    );
    assert_eq!(
        IndicatorParams::parse(r#"{"period": nan}"#).unwrap_err(),
        r#"Invalid params JSON: {"period": nan}"#
    );
}

#[test]
fn a_tail_is_always_the_length_asked_for() {
    let records = closes(120);
    let params = IndicatorParams::default();
    let IndicatorOutput::Success(out) = run(
        "SMA ",
        &params,
        IndicatorSource::Candles {
            records: &records,
            lookback_bars: 100,
        },
    ) else {
        panic!("sma computes");
    };
    assert_eq!(out.indicator, "sma");
    assert_eq!(out.series.len(), 5);
    assert_eq!(out.series[4], out.latest);
    assert_eq!(out.series[3], out.previous);
    assert_eq!(out.at_offset, out.latest);
    assert_eq!(out.params.pairs(), &[("period".to_string(), json!(14))]);
    assert_eq!(out.bars_used, 120);
}
