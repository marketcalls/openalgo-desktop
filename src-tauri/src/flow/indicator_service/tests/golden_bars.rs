//! `bar_at_offset`, `prior_period_ohlc` and `_parse_timestamp` against
//! `bars.json` (the web's answers with a frozen IST "now"). The Bar Offset
//! cases cover every interval key at both edges of its forming window
//! (IND-005, IND-061).
//!
//! One deliberate difference is pinned next to the web's answer (IND-062):
//! the web treats a weekly or monthly bar as forming only on the date its
//! timestamp carries, so from the second day of a week (or month) offset 0
//! returns the still-forming bar; the desktop drops it while its week or
//! month contains "now".

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};
use serde_json::Value;

use super::support::{diff, error_diff, json, records, wall_time, Failures, BARS};
use crate::flow::indicator_service::records::parse_timestamp;
use crate::flow::indicator_service::{bar_at_offset, prior_period_ohlc};

fn rows_of(case: &Value) -> Vec<Value> {
    match case.get("records") {
        Some(rows) => rows.as_array().expect("records").clone(),
        None => records(&case["candles"]),
    }
}

/// The calendar bucket of a weekly (Monday to Sunday) or monthly bar.
fn bucket(at: NaiveDateTime, weekly: bool) -> NaiveDate {
    let day = at.date();
    if weekly {
        day - Duration::days(i64::from(day.weekday().num_days_from_monday()))
    } else {
        NaiveDate::from_ymd_opt(day.year(), day.month(), 1).expect("first of month")
    }
}

/// True when the web keeps a bar the desktop drops as forming: a weekly or
/// monthly last bar in now's week or month, dated another day.
fn forming_only_here(rows: &[Value], interval: &str, now: NaiveDateTime) -> bool {
    let weekly = match interval.trim().to_lowercase().as_str() {
        "w" | "week" => true,
        "m" | "month" => false,
        _ => return false,
    };
    let Some(last) = rows.last() else {
        return false;
    };
    let Ok(at) = parse_timestamp(last.get("timestamp")) else {
        return false;
    };
    bucket(at, weekly) == bucket(now, weekly) && at.date() != now.date()
}

#[test]
fn bar_at_offset_matches_the_web_except_the_forming_week_or_month() {
    let fixture = json(BARS);
    let cases = fixture["bar_at_offset"].as_array().expect("cases");
    let mut failures = Failures::default();
    let mut differing = 0;
    for case in cases {
        let rows = rows_of(case);
        let now = wall_time(&case["now"]);
        let interval = case["interval"].as_str().expect("interval");
        let offset = case["offset"].as_i64().expect("offset");
        let got = bar_at_offset(&rows, offset, interval, now);
        let label = format!("{} {interval:?} offset {offset}", case["label"]);
        let at = |back: i64| -> Value {
            usize::try_from(rows.len() as i64 - 1 - back)
                .ok()
                .and_then(|i| rows.get(i))
                .cloned()
                .unwrap_or(Value::Null)
        };
        match (&got, case.get("error")) {
            (Err(e), Some(want)) => {
                if let Some(problem) = error_diff(e, want) {
                    failures.push(&label, problem);
                }
                continue;
            }
            (Err(e), None) => {
                failures.push(&label, format!("raised {e:?}"));
                continue;
            }
            (Ok(_), Some(want)) => {
                failures.push(&label, format!("the web raised {want}"));
                continue;
            }
            (Ok(_), None) => {}
        }
        let got = got.ok().flatten().cloned().unwrap_or(Value::Null);
        let web = &case["result"];
        if offset >= 0 && forming_only_here(&rows, interval, now) {
            // The web counts the forming bar as offset 0; the desktop
            // starts from the bar before it.
            differing += 1;
            if *web != at(offset) || got != at(offset + 1) {
                failures.push(&label, format!("web {web} ours {got}"));
            }
        } else if got != *web {
            failures.push(&label, format!("{got} != {web}"));
        }
    }
    assert!(differing > 0, "no case exercises the forming week or month");
    failures.check(cases.len());
}

#[test]
fn prior_period_ohlc_matches_the_web() {
    let fixture = json(BARS);
    let cases = fixture["prior_period_ohlc"].as_array().expect("cases");
    let mut failures = Failures::default();
    for case in cases {
        let rows = rows_of(case);
        let period = case["period"].as_str().expect("period");
        let got = prior_period_ohlc(&rows, period, wall_time(&case["now"]));
        let label = format!("{} {} {period}", case["candles"], case["now"]);
        let problem = match (&got, case.get("result"), case.get("error")) {
            (Ok(out), Some(want), _) => diff(&Value::Object(out.to_map()), want),
            (Err(e), _, Some(want)) => error_diff(e, want),
            (other, _, want) => Some(format!("{other:?} vs {want:?}")),
        };
        if let Some(problem) = problem {
            failures.push(label, problem);
        }
    }
    failures.check(cases.len());
}

/// `_parse_timestamp` (IND-060): epoch numbers and epoch text in IST, ISO
/// text with `Z` or any offset converted to IST wall time, naive ISO text as
/// written, and Python's own sentence for what it cannot read.
#[test]
fn parse_timestamp_matches_the_web() {
    let fixture = json(BARS);
    let cases = fixture["parse_timestamp"].as_array().expect("cases");
    let mut failures = Failures::default();
    for case in cases {
        let value = &case["value"];
        let got = parse_timestamp(Some(value).filter(|v| !v.is_null()));
        let problem = match (&got, case.get("result"), case.get("error")) {
            (Ok(at), Some(want), _) => {
                let shown = if at.and_utc().timestamp_subsec_micros() == 0 {
                    at.format("%Y-%m-%dT%H:%M:%S").to_string()
                } else {
                    at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
                };
                (want.as_str() != Some(shown.as_str())).then(|| format!("{shown} != {want}"))
            }
            (Err(e), _, Some(want)) => error_diff(e, want),
            (other, _, _) => Some(format!("{other:?} vs {case}")),
        };
        if let Some(problem) = problem {
            failures.push(value, problem);
        }
    }
    failures.check(cases.len());
}
