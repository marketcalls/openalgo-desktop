//! The bar and calendar ceilings against `window.json`: `clamp_bars`,
//! `calendar_days_for_bars`, `history_window` (today frozen at the
//! recorder's date), `clamp_history_range` and `trim_records`.

use chrono::NaiveDate;
use serde_json::{json as value, Value};

use super::support::{json, Failures, WINDOW};
use crate::flow::indicator_service::{
    calendar_days_for_bars, clamp_bars, clamp_history_range, history_window, trim_records,
};

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("date")
}

fn text(v: &Value) -> &str {
    v.as_str().expect("text")
}

#[test]
fn clamp_bars_and_calendar_days_match_the_web() {
    let fixture = json(WINDOW);
    let mut failures = Failures::default();
    let mut checked = 0;
    for case in fixture["clamp_bars"].as_array().expect("clamp") {
        checked += 1;
        let got = clamp_bars(case["requested"].as_i64().expect("n"), "bars");
        if value!(got) != case["result"] {
            failures.push(format!("clamp_bars {}", case["requested"]), got);
        }
    }
    for case in fixture["calendar_days"].as_array().expect("calendar") {
        checked += 1;
        let got = calendar_days_for_bars(
            case["bars"].as_i64().expect("bars"),
            text(&case["interval"]),
        );
        if value!(got) != case["result"] {
            failures.push(
                format!("calendar_days {} {}", case["bars"], case["interval"]),
                got,
            );
        }
    }
    failures.check(checked);
}

#[test]
fn history_window_matches_the_web() {
    let fixture = json(WINDOW);
    let today = date(text(&fixture["today"]));
    let mut failures = Failures::default();
    let cases = fixture["history_window"].as_array().expect("window");
    for case in cases {
        let end = case.get("end").map_or(today, |e| date(text(e)));
        let (start, stop) = history_window(
            case["lookback_bars"].as_i64().expect("bars"),
            case["lookback_days"].as_i64(),
            text(&case["interval"]),
            end,
        );
        if value!([start, stop]) != case["result"] {
            failures.push(format!("{case}"), format!("{start}..{stop}"));
        }
    }
    failures.check(cases.len());
}

#[test]
fn clamp_history_range_matches_the_web() {
    let fixture = json(WINDOW);
    let mut failures = Failures::default();
    let cases = fixture["clamp_history_range"].as_array().expect("ranges");
    for case in cases {
        let (start, end) = clamp_history_range(
            text(&case["start"]),
            text(&case["end"]),
            text(&case["interval"]),
        );
        if value!([start, end]) != case["result"] {
            failures.push(format!("{case}"), format!("{start}..{end}"));
        }
    }
    failures.check(cases.len());
}

#[test]
fn trim_records_keeps_the_latest_200() {
    let fixture = json(WINDOW);
    let cases = fixture["trim_records"].as_array().expect("trim");
    for case in cases {
        let count = case["count"].as_u64().expect("count") as usize;
        let rows: Vec<Value> = (0..count).map(|i| value!({ "i": i })).collect();
        let kept = trim_records(&rows);
        assert_eq!(value!(kept.len()), case["kept"], "{case}");
        let first = kept.first().map_or(Value::Null, |r| r["i"].clone());
        assert_eq!(first, case["first"], "{case}");
    }
}
