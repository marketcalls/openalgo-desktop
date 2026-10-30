//! `compute_indicator` against the web's answers in `compute.json`: every
//! indicator on every candle shape, the lookback, tail and offset
//! arithmetic, parameter binding, nested mode and every refusal.
//!
//! The web's numbers are compared with the web's reading of the rows
//! ([`compute_indicator_as_web`]); the desktop reads rows by bar instead
//! (IND-049), and `by_bar_differs_only_where_an_output_has_a_gap` pins where
//! and why the two differ.

use serde_json::Value;

use super::support::{diff, error_diff, json, numbers, pairs, records, Failures, COMPUTE};
use crate::flow::indicator_service::compute::compute_indicator_as_web;
use crate::flow::indicator_service::{
    compute_indicator, IndicatorError, IndicatorOutput, IndicatorParams, IndicatorRequest,
    IndicatorSource,
};

/// One recorded case, run through `run` with the case's own request.
fn with_request<T>(case: &Value, run: impl FnOnce(&IndicatorRequest<'_>) -> T) -> T {
    let params = IndicatorParams::from_pairs(pairs(&case["params"]));
    let nested;
    let candles;
    let source = match case.get("nested") {
        Some(values) => {
            nested = numbers(values);
            IndicatorSource::Nested(&nested)
        }
        None => {
            candles = with_timestamps(records(&case["candles"]), case.get("timestamps"));
            IndicatorSource::Candles {
                records: &candles,
                lookback_bars: case["lookback_bars"].as_i64().expect("lookback"),
            }
        }
    };
    run(&IndicatorRequest {
        indicator_name: case["name"].as_str().expect("name"),
        params: &params,
        source,
        tail_bars: case["tail_bars"].as_i64().expect("tail"),
        offset_bars: case["offset_bars"].as_i64().expect("offset"),
    })
}

/// The records with the case's own timestamp column, when it has one
/// (`{"missing": true}` drops the key).
fn with_timestamps(mut rows: Vec<Value>, stamps: Option<&Value>) -> Vec<Value> {
    let Some(stamps) = stamps.and_then(Value::as_array) else {
        return rows;
    };
    for (row, stamp) in rows.iter_mut().zip(stamps) {
        let Some(row) = row.as_object_mut() else {
            continue;
        };
        if stamp.get("missing").is_some() {
            row.remove("timestamp");
        } else {
            row.insert("timestamp".into(), stamp.clone());
        }
    }
    rows
}

fn label(case: &Value) -> String {
    format!(
        "[{} {}] {} {} {}",
        case["tag"].as_str().unwrap_or_default(),
        case["label"].as_str().unwrap_or_default(),
        case["name"],
        case["params"],
        case.get("candles")
            .or(case.get("nested").map(|_| &Value::Null))
            .unwrap_or(&Value::Null)
    )
}

/// The first difference between an answer and the web's (`result` or
/// `error`).
pub(super) fn answer_diff(
    got: &Result<IndicatorOutput, IndicatorError>,
    case: &Value,
) -> Option<String> {
    match (got, case.get("result"), case.get("error")) {
        (Ok(out), Some(want), _) => diff(&out.to_value(), want),
        (Err(e), _, Some(want)) => error_diff(e, want),
        (Ok(out), None, Some(want)) => Some(format!(
            "computed {} but the web raised {want}",
            out.to_value()
        )),
        (Err(e), Some(_), None) => Some(format!("raised {e:?} but the web computed")),
        _ => Some("malformed case".into()),
    }
}

/// Every case, word for word and number for number: every refusal, every
/// value of the wrong type (IND-042) and every timestamp column (IND-045)
/// included.
#[test]
fn every_case_matches_the_web() {
    let fixture = json(COMPUTE);
    let cases = fixture["cases"].as_array().expect("cases");
    let mut failures = Failures::default();
    for case in cases {
        let got = with_request(case, compute_indicator_as_web);
        let problem = answer_diff(&got, case);
        if let Some(problem) = problem {
            failures.push(label(case), problem);
        }
    }
    failures.check(cases.len());
}

/// The rows the desktop reports differ from the web's only for outputs
/// with a gap after their first value (the web drops it per output, so its
/// rows mix bars). Measured on the recorded cases: ichimoku's lagging span
/// everywhere, and the cumulative volume indicators and `fisher` on candles
/// whose first bars are missing, where the web reports the one value on the
/// first bar as `latest` and the desktop reports no value.
#[test]
fn by_bar_differs_only_where_an_output_has_a_gap() {
    let fixture = json(COMPUTE);
    let mut differing: Vec<String> = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let (Ok(ours), Ok(web)) = (
            with_request(case, compute_indicator),
            with_request(case, compute_indicator_as_web),
        ) else {
            continue;
        };
        let name = case["name"].as_str().unwrap_or_default().to_string();
        if ours != web && !differing.contains(&name) {
            differing.push(name);
        }
    }
    differing.sort();
    assert_eq!(
        differing,
        ["adl", "fisher", "ichimoku", "obv", "obv_smoothed", "pvt"]
    );
}

/// With no volume on any bar, `obv` has a value only on the first bar. The
/// web reports that value, 150 bars old, as `latest`; the desktop reports
/// that the indicator produced no value.
#[test]
fn a_first_bar_value_is_not_reported_as_latest() {
    let fixture = json(COMPUTE);
    let case = fixture["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|c| {
            c["name"] == "obv"
                && c["candles"]["fill"]["volume"].is_null()
                && c["candles"]["fill"].is_object()
        })
        .expect("obv without volume");
    assert_eq!(case["result"]["latest"], serde_json::json!({"value": 0.0}));
    let web = with_request(case, compute_indicator_as_web).expect("computes");
    assert!(matches!(web, IndicatorOutput::Success(_)));
    let ours = with_request(case, compute_indicator).expect("computes");
    let IndicatorOutput::NoValue(no_value) = ours else {
        panic!("expected no value, got {ours:?}");
    };
    assert!(no_value
        .message
        .starts_with("Indicator 'obv' produced no value over 150 bars"));
}

/// Ichimoku's lagging span (`out4`) is the close plotted 26 bars back, so
/// it has no value on the last 25 bars. The web's `latest.out4` is the
/// latest close, which sits on the bar 25 bars before the one `out0..out3`
/// describe in the same row; the desktop reports `None` for the latest bar.
#[test]
fn ichimokus_lagging_span_is_none_on_the_latest_bar() {
    let fixture = json(COMPUTE);
    let case = fixture["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|c| {
            c["tag"] == "sweep" && c["name"] == "ichimoku" && c["candles"]["shape"] == "trend"
        })
        .expect("ichimoku on trend");
    let web_latest = case["result"]["latest"]["out4"]
        .as_f64()
        .expect("the web reports a value");
    let candles = records(&case["candles"]);
    let last_close = candles[candles.len() - 1]["close"].as_f64().expect("close");
    assert!(
        (web_latest - last_close).abs() < 1e-6,
        "{web_latest} vs {last_close}"
    );
    let IndicatorOutput::Success(ours) = with_request(case, compute_indicator).expect("computes")
    else {
        panic!("ichimoku computes");
    };
    let out4 =
        |row: &[(String, Option<f64>)]| row.iter().find(|(n, _)| n == "out4").and_then(|(_, v)| *v);
    assert_eq!(out4(&ours.latest), None);
    assert!(ours
        .latest
        .iter()
        .filter(|(n, _)| n != "out4")
        .all(|(_, v)| v.is_some()));
    // 25 bars back, both readings meet: the lagging span's latest value.
    assert_eq!(out4(&ours.at_offset), None);
    let IndicatorOutput::Success(back) = with_request(
        &serde_json::json!({
            "name": "ichimoku", "params": [], "candles": case["candles"], "lookback_bars": 100,
            "tail_bars": 5, "offset_bars": 25
        }),
        compute_indicator,
    )
    .expect("computes") else {
        panic!("ichimoku computes");
    };
    assert_eq!(out4(&back.at_offset), Some(web_latest));
}
