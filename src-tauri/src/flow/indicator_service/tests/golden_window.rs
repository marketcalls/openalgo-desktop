//! The bar and calendar ceilings against `window.json`: `clamp_bars`,
//! `calendar_days_for_bars`, `history_window` (today frozen at the
//! recorder's date), `clamp_history_range` and `trim_records`.

use chrono::NaiveDate;
use serde_json::{json as value, Value};

use super::support::{json, Failures, WINDOW};
use crate::flow::indicator_service::{
    calendar_days_for_bars, clamp_bars, clamp_history_range, history_window, trim_records,
};
use crate::services::tunnel::fake::capture_logs;

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

/// The ceilings write the web's server log lines (decision 35): the bar
/// clamp, the trim, the calendar cap and the range narrowing, at INFO, and
/// nothing when no ceiling is reached (`window.json` `logged`, recorded
/// from the web's logger).
#[test]
fn the_ceilings_log_the_webs_lines() {
    let fixture = json(WINDOW);
    let today = date(text(&fixture["today"]));
    let cases = fixture["logged"].as_array().expect("logged");
    for case in cases {
        let args = &case["args"];
        let int = |i: usize| args[i].as_i64().expect("int");
        let logs = capture_logs();
        match text(&case["call"]) {
            "clamp_bars" => {
                clamp_bars(int(0), text(&args[1]));
            }
            "trim_records" => {
                let rows: Vec<Value> = (0..int(0)).map(|i| value!({ "i": i })).collect();
                trim_records(&rows);
            }
            "history_window" => {
                history_window(int(0), args[1].as_i64(), text(&args[2]), today);
            }
            "clamp_history_range" => {
                clamp_history_range(text(&args[0]), text(&args[1]), text(&args[2]));
            }
            other => panic!("unknown call {other}"),
        }
        let lines: Vec<String> = logs
            .text()
            .lines()
            .filter(|line| line.contains("indicator_service"))
            .map(str::to_string)
            .collect();
        let want = case["lines"].as_array().expect("lines");
        assert_eq!(lines.len(), want.len(), "{case}: {lines:?}");
        for (line, expected) in lines.iter().zip(want) {
            let level = text(&expected[0]);
            let message = text(&expected[1]);
            assert!(
                line.contains(level) && line.ends_with(message),
                "{line:?} vs {expected}"
            );
        }
    }
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
