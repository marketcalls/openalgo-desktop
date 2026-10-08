//! Schedules: when a deployment starts and stops on its own, in IST (web
//! `blueprints/openscript_runner.py`, `set_schedule` and the cron jobs).
//!
//! The web registers two cron jobs per schedule on the strategy host's
//! scheduler. Here one owned task ticks against the injected clock and asks
//! [`due`] which starts and stops fell inside the span since its last tick,
//! so tests drive a schedule by moving the clock.

use super::store::Schedule;
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The days a schedule may name, in week order.
pub const DAYS: &[&str] = &["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// A start found this late is not acted on: a strategy started an hour into
/// the session because the app was asleep is not what the schedule said.
pub const START_GRACE: Duration = Duration::minutes(5);

fn hhmm(text: &str) -> Option<NaiveTime> {
    let b = text.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return None;
    }
    let h: u32 = text[..2].parse().ok()?;
    let m: u32 = text[3..].parse().ok()?;
    if !b[..2].iter().chain(&b[3..]).all(u8::is_ascii_digit) {
        return None;
    }
    NaiveTime::from_hms_opt(h, m, 0).filter(|_| h < 24 && m < 60)
}

/// Check a schedule body the way the web does. `Err` is the trader's sentence.
pub fn parse(body: &Map<String, Value>) -> Result<Schedule, String> {
    let unknown: Vec<&str> = body
        .keys()
        .map(String::as_str)
        .filter(|k| !matches!(*k, "start_time" | "stop_time" | "days"))
        .collect();
    if !unknown.is_empty() {
        let mut u = unknown;
        u.sort();
        return Err(format!(
            "A schedule has a start time, a stop time and days. This one also carried {}. The exchange comes from the script's own run settings.",
            u.join(", ")
        ));
    }
    let start = match body.get("start_time") {
        Some(Value::String(s)) if hhmm(s).is_some() => s.clone(),
        _ => return Err("Give a start time as 24 hour HH:MM in IST.".into()),
    };
    let stop = match body.get("stop_time") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if hhmm(s).is_some() => Some(s.clone()),
        _ => return Err("Give a stop time as 24 hour HH:MM in IST, or leave it out.".into()),
    };
    if stop.as_deref().is_some_and(|s| s <= start.as_str()) {
        return Err("The stop time is before the start time. Check both.".into());
    }
    let days: Vec<String> = match body.get("days") {
        None | Some(Value::Null) => DAYS[..5].iter().map(|d| d.to_string()).collect(),
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .map(|d| match d {
                Value::String(s) => s.trim().to_ascii_lowercase(),
                other => other.to_string().trim().to_ascii_lowercase(),
            })
            .collect(),
        _ => return Err("Give the days to run on, or leave them out.".into()),
    };
    let mut wrong: Vec<&String> = days
        .iter()
        .filter(|d| !DAYS.contains(&d.as_str()))
        .collect();
    if !wrong.is_empty() {
        wrong.sort();
        wrong.dedup();
        return Err(format!(
            "These are not days of the week: {}.",
            wrong
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(Schedule {
        start_time: start,
        stop_time: stop,
        days: DAYS
            .iter()
            .filter(|d| days.iter().any(|x| x == *d))
            .map(|d| d.to_string())
            .collect(),
    })
}

/// What a schedule asks for at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    Stop,
    Start,
}

fn weekday_name(d: NaiveDate) -> &'static str {
    DAYS[d.weekday().num_days_from_monday() as usize]
}

fn instant(date: NaiveDate, at: &str) -> Option<DateTime<Utc>> {
    let t = hhmm(at)?;
    Kolkata
        .from_local_datetime(&date.and_time(t))
        .single()
        .map(|d| d.with_timezone(&Utc))
}

/// The starts and stops that fell in `(from, to]`, stops first, each once.
/// The date it fell on is returned with it, for the trading calendar.
pub fn due(
    schedules: &BTreeMap<String, Schedule>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<(String, Action, NaiveDate)> {
    if to <= from {
        return Vec::new();
    }
    // Never look further back than two days, whatever the gap.
    let from = from.max(to - Duration::days(2));
    let first = from.with_timezone(&Kolkata).date_naive();
    let last = to.with_timezone(&Kolkata).date_naive();
    let mut out: BTreeMap<(String, Action), NaiveDate> = BTreeMap::new();
    for (name, s) in schedules {
        let mut d = first;
        while d <= last {
            if s.days.iter().any(|x| x == weekday_name(d)) {
                if let Some(at) = instant(d, &s.start_time) {
                    if at > from && at <= to && to - at <= START_GRACE {
                        out.insert((name.clone(), Action::Start), d);
                    }
                }
                if let Some(at) = s.stop_time.as_deref().and_then(|t| instant(d, t)) {
                    if at > from && at <= to {
                        out.insert((name.clone(), Action::Stop), d);
                    }
                }
            }
            match d.succ_opt() {
                Some(n) => d = n,
                None => break,
            }
        }
    }
    let mut list: Vec<(String, Action, NaiveDate)> =
        out.into_iter().map(|((n, a), d)| (n, a, d)).collect();
    list.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn ist(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Kolkata
            .with_ymd_and_hms(2026, 10, d, h, m, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn bodies_are_checked_like_the_web() {
        let s = parse(&body(json!({"start_time": "09:20"}))).unwrap();
        assert_eq!(s.days, vec!["mon", "tue", "wed", "thu", "fri"]);
        assert_eq!(s.stop_time, None);
        let s = parse(&body(
            json!({"start_time": "09:20", "stop_time": "15:10", "days": ["SUN", "mon"]}),
        ))
        .unwrap();
        assert_eq!(s.days, vec!["mon", "sun"]);
        assert!(parse(&body(json!({"start_time": "9:20"}))).is_err());
        assert!(parse(&body(json!({"start_time": "24:00"}))).is_err());
        assert!(
            parse(&body(json!({"start_time": "10:00", "stop_time": "09:00"})))
                .unwrap_err()
                .contains("before the start")
        );
        assert!(
            parse(&body(json!({"start_time": "10:00", "days": ["funday"]})))
                .unwrap_err()
                .contains("funday")
        );
        assert!(
            parse(&body(json!({"start_time": "10:00", "exchange": "NSE"})))
                .unwrap_err()
                .contains("exchange")
        );
        assert!(parse(&body(json!({"start_time": "10:00", "days": []}))).is_err());
    }

    #[test]
    fn due_finds_each_crossing_once_and_skips_stale_starts() {
        let mut all = BTreeMap::new();
        all.insert(
            "openscript_t".to_string(),
            Schedule {
                start_time: "09:20".into(),
                stop_time: Some("15:10".into()),
                days: vec!["mon".into()],
            },
        );
        // 2026-10-05 is a Monday.
        assert_eq!(
            due(&all, ist(5, 9, 19), ist(5, 9, 20)),
            vec![(
                "openscript_t".into(),
                Action::Start,
                NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()
            )]
        );
        assert!(due(&all, ist(5, 9, 20), ist(5, 9, 21)).is_empty());
        // Woke up an hour late: no start, but the stop still fires later.
        assert!(due(&all, ist(5, 9, 0), ist(5, 10, 20)).is_empty());
        assert_eq!(due(&all, ist(5, 15, 0), ist(5, 15, 30))[0].1, Action::Stop);
        // Tuesday is not on the list.
        assert!(due(&all, ist(6, 9, 19), ist(6, 9, 21)).is_empty());
    }
}
