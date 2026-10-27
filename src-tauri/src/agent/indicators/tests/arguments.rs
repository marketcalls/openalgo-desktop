//! The web's own tests of the indicator arguments and the scan, ported
//! (`test/test_agent_viz_tool_arguments.py`, `TestIndicatorParameterAliases`),
//! plus the batch and scan cores.

use serde_json::{json as j, Map, Value};

use super::support::{frame, json, COMPUTE};
use crate::agent::indicators::coerce::resolve_param_aliases;
use crate::agent::indicators::compute::{compute, Cell};
use crate::agent::indicators::compute_batch;
use crate::agent::indicators::frame::CandleFrame;
use crate::agent::indicators::registry::get_spec;
use crate::agent::indicators::scan::{
    evaluate_condition, parse_condition, plan_scan, scan_symbol, ResultView,
};

fn trend() -> CandleFrame {
    frame(&json(COMPUTE)["frames"]["trend"])
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object")
}

fn resolved(indicator: &str, given: Value) -> Value {
    let spec = get_spec(indicator).expect("spec");
    Value::Object(
        resolve_param_aliases(spec, Some(&object(given)))
            .into_iter()
            .collect(),
    )
}

#[test]
fn a_borrowed_name_maps_to_the_indicators_own() {
    assert_eq!(resolved("rsi", j!({"length": 21})), j!({"period": 21}));
    assert_eq!(
        resolved("macd", j!({"fast": 8, "slow": 21, "signal": 5})),
        j!({"fast_period": 8, "slow_period": 21, "signal_period": 5})
    );
    assert_eq!(resolved("bbands", j!({"std": 2.5})), j!({"std_dev": 2.5}));
    assert_eq!(resolved("kama", j!({"fast": 3})), j!({"fast_length": 3}));
    assert_eq!(resolved("rsi", j!({"Period": 14})), j!({"period": 14}));
}

#[test]
fn a_real_parameter_is_never_renamed() {
    assert_eq!(resolved("kama", j!({"length": 10})), j!({"length": 10}));
}

#[test]
fn compute_runs_with_an_alias_and_reports_the_real_name() {
    let given = object(j!({"length": 21}));
    let result = compute("rsi", &trend(), Some(&given), None, 3).expect("computes");
    let shown = serde_json::to_value(result.to_json()).expect("json");
    assert_eq!(shown["params_used"]["period"], 21);
    assert_eq!(shown["defaults_applied"], Value::Null);
    assert_eq!(shown["values"]["rsi"].as_array().map(Vec::len), Some(3));
}

#[test]
fn an_alias_next_to_its_real_name_is_still_refused() {
    let given = object(j!({"length": 21, "period": 14}));
    let error = compute("rsi", &trend(), Some(&given), None, 3).expect_err("refused");
    assert!(error.0.contains("no parameter 'length'"), "{}", error.0);
}

#[test]
fn a_name_that_is_no_alias_is_still_refused() {
    let given = object(j!({"window": 21}));
    let error = compute("rsi", &trend(), Some(&given), None, 3).expect_err("refused");
    assert!(error.0.contains("no parameter 'window'"), "{}", error.0);
}

fn cells(values: &[f64]) -> Vec<Cell> {
    values.iter().map(|v| Cell::Num(*v)).collect()
}

#[test]
fn a_condition_matches_output_names_case_insensitively() {
    let values = vec![("rsi".to_string(), cells(&[40.0, 25.0]))];
    let view = ResultView {
        indicator: "rsi",
        outputs: None,
        values: &values,
    };
    let (hit, detail) = evaluate_condition(&parse_condition("RSI < 30").expect("parses"), &view);
    assert!(hit);
    assert_eq!(
        detail.get("condition_met"),
        Some(&crate::agent::indicators::json::Json::Bool(true))
    );

    let macd = vec![
        ("macd_line".to_string(), cells(&[1.0])),
        ("signal_line".to_string(), cells(&[0.5])),
    ];
    let view = ResultView {
        indicator: "macd",
        outputs: None,
        values: &macd,
    };
    assert!(evaluate_condition(&parse_condition("MACD_LINE > 0").expect("parses"), &view).0);
}

#[test]
fn an_output_the_indicator_does_not_have_still_fails() {
    let values = vec![("rsi".to_string(), cells(&[25.0]))];
    let outputs = vec!["rsi".to_string()];
    let view = ResultView {
        indicator: "rsi",
        outputs: Some(&outputs),
        values: &values,
    };
    let (hit, detail) = evaluate_condition(&parse_condition("ADX < 30").expect("parses"), &view);
    assert!(!hit);
    let text = detail.to_text();
    assert!(text.contains("no output named"), "{text}");
}

#[test]
fn a_scan_plan_refuses_what_the_web_refuses() {
    assert!(plan_scan("RSI", "RSI < 101").is_ok());
    let unparsed = plan_scan("rsi", "RSI between 20 and 30").expect_err("refused");
    assert_eq!(
        unparsed.message(),
        "The 'condition' argument is invalid: 'RSI between 20 and 30' is not one of the two \
         accepted shapes Use a comparison against a number, such as \"rsi < 30\" or \"adx > 25\", \
         or a cross between two of the indicator's own outputs, such as \
         \"crossover(macd_line, signal_line)\". Nothing else is accepted: this is a fixed parser, \
         not an expression evaluator. Valid output names for rsi: rsi."
    );
    let pair = plan_scan("beta", "beta > 1").expect_err("refused");
    assert_eq!(
        pair.message(),
        "The 'indicator' argument is invalid: beta compares two instruments and cannot be scanned \
         across a list Use compute_indicator with compare_symbol instead, once per instrument."
    );
    assert!(plan_scan("nope", "x > 1")
        .expect_err("unknown")
        .message()
        .contains("'nope'"));
}

#[test]
fn a_scan_evaluates_one_instrument_over_five_values() {
    let plan = plan_scan("rsi", "RSI < 101").expect("plan");
    let given = object(j!({"length": 14}));
    let (hit, detail) = scan_symbol(&plan, &trend(), Some(&given)).expect("scans");
    assert!(hit);
    let detail = serde_json::to_value(detail).expect("json");
    assert_eq!(detail["condition_met"], true);
    assert!(detail["RSI"]
        .as_f64()
        .is_some_and(|v| (0.0..=100.0).contains(&v)));

    let plan = plan_scan("macd", "crossover(macd_line, signal_line)").expect("plan");
    let (_, detail) = scan_symbol(&plan, &trend(), None).expect("scans");
    let detail = serde_json::to_value(detail).expect("json");
    assert!(
        detail["macd_line"].is_f64() && detail["signal_line"].is_f64(),
        "{detail}"
    );
}

#[test]
fn a_batch_keeps_what_ran_and_names_what_did_not() {
    let requests = vec![
        ("rsi".to_string(), Map::new()),
        ("rsi".to_string(), object(j!({"period": 5}))),
        ("vwma".to_string(), object(j!({"period": 9999}))),
        ("crossover".to_string(), Map::new()),
    ];
    let outcome = compute_batch(&trend(), &requests, 4);
    assert_eq!(outcome.results.len(), 2);
    assert_eq!(outcome.failures.len(), 2);
    assert_eq!(outcome.failures[0].0, "vwma");
    assert!(outcome.failures[0]
        .1
        .starts_with("vwma: a period of 9999 needs more than"));
    assert!(outcome.all_failed().is_none());

    let none = compute_batch(&trend(), &requests[2..], 4);
    let joined = none.all_failed().expect("all failed");
    assert!(
        joined.contains("; crossover cannot be computed directly."),
        "{joined}"
    );
}
