//! The trading calendar (web `test/test_trading_calendar.py`, ported case
//! for case over the same fixed calendar), the settlement-holiday fix
//! (N-calendar-05), `date.fromisoformat` and the Calendar node's IST
//! session date on a machine outside IST (N-dataCommon-10).

use super::support::{ctx_at, env_with, logs, run, FakeBook, FakeCalendar, ServerLog};
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::nodes::calendar::{from_isoformat, Period, TradingDays};
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap_or_else(|| panic!("bad date"))
}

/// The web test's fixture: Republic Day (a Monday), 31 Mar and 1 Apr 2026
/// closed, and a Sunday Muhurat session on 8 Nov 2026.
fn fixed() -> FakeCalendar {
    let closed = |date: &str| json!({"date": date, "holiday_type": "TRADING_HOLIDAY", "closed_exchanges": ["NSE"]});
    let mut years = HashMap::new();
    years.insert(
        2026,
        Ok(vec![
            closed("2026-01-26"),
            closed("2026-03-31"),
            closed("2026-04-01"),
            json!({"date": "2026-11-08", "holiday_type": "SPECIAL_SESSION", "closed_exchanges": []}),
        ]),
    );
    FakeCalendar(years)
}

const ALL: [Period; 5] = [
    Period::Day,
    Period::Week,
    Period::Month,
    Period::Quarter,
    Period::Year,
];

#[test]
fn weekends_and_holidays_are_not_trading_days() {
    let cal = fixed();
    let t = TradingDays::new(&cal);
    assert!(!t.is_trading_day(d(2026, 8, 1)));
    assert!(!t.is_trading_day(d(2026, 8, 2)));
    assert!(t.is_trading_day(d(2026, 8, 3)));
    assert!(!t.is_trading_day(d(2026, 1, 26)));
    assert!(t.is_trading_day(d(2026, 1, 27)));
}

#[test]
fn new_periods_survive_holidays_and_weekends() {
    let cal = fixed();
    let t = TradingDays::new(&cal);
    // A holiday Monday: the Tuesday opens the week.
    assert!(!t.is_first_of(d(2026, 1, 26), Period::Week));
    assert!(t.is_first_of(d(2026, 1, 27), Period::Week));
    // 1 Aug 2026 is a Saturday.
    assert!(!t.is_first_of(d(2026, 8, 1), Period::Month));
    assert!(t.is_first_of(d(2026, 8, 3), Period::Month));
    // 1 Apr 2026 is a holiday here, so Q2 opens on the 2nd.
    assert!(!t.is_first_of(d(2026, 4, 1), Period::Quarter));
    assert!(t.is_first_of(d(2026, 4, 2), Period::Quarter));
    assert!(t.is_first_of(d(2026, 4, 2), Period::Month));
    assert!(t.is_first_of(d(2026, 1, 1), Period::Year));
    assert!(!t.is_first_of(d(2026, 1, 2), Period::Year));
    // 31 Mar 2026 is a holiday here, so March closes on the 30th.
    assert!(!t.is_last_of(d(2026, 3, 31), Period::Month));
    assert!(t.is_last_of(d(2026, 3, 30), Period::Month));
    for p in ALL {
        assert!(!t.is_first_of(d(2026, 8, 2), p), "{:?}", p);
        assert!(!t.is_last_of(d(2026, 8, 2), p), "{:?}", p);
    }
}

#[test]
fn adjacent_trading_days_skip_weekends_holidays_and_find_special_sessions() {
    let cal = fixed();
    let t = TradingDays::new(&cal);
    assert_eq!(t.prev_trading_day(d(2026, 8, 3)), d(2026, 7, 31));
    assert_eq!(t.next_trading_day(d(2026, 7, 31)), d(2026, 8, 3));
    assert_eq!(t.next_trading_day(d(2026, 1, 23)), d(2026, 1, 27));
    assert_eq!(t.next_trading_day(d(2026, 11, 7)), d(2026, 11, 8));
    assert_eq!(t.prev_trading_day(d(2026, 11, 9)), d(2026, 11, 8));
}

#[test]
fn describe_covers_every_flag_a_workflow_reads() {
    let cal = fixed();
    let t = TradingDays::new(&cal);
    let info = t.describe(d(2026, 8, 3));
    for key in [
        "is_trading_day",
        "is_trading_holiday",
        "is_weekend",
        "weekday",
        "weekday_num",
        "quarter",
        "week_of_year",
        "day_of_year",
        "is_new_day",
        "is_new_week",
        "is_new_month",
        "is_new_quarter",
        "is_new_year",
        "is_last_day_of_week",
        "is_last_day_of_month",
        "is_last_day_of_quarter",
        "prev_trading_day",
        "next_trading_day",
    ] {
        assert!(info.contains_key(key), "{}", key);
    }
    assert_eq!(info.len(), 30, "exactly the web's keys");
    assert_eq!(info["weekday_num"], 1);
    assert_eq!(info["quarter"], 3);
    for (day, quarter) in [
        ((1, 15), 1),
        ((3, 31), 1),
        ((4, 1), 2),
        ((7, 30), 3),
        ((12, 31), 4),
    ] {
        assert_eq!(t.describe(d(2026, day.0, day.1))["quarter"], quarter);
    }
    let holiday = t.describe(d(2026, 1, 26));
    let weekend = t.describe(d(2026, 8, 2));
    assert_eq!(
        (
            holiday["is_trading_holiday"].clone(),
            holiday["is_weekend"].clone()
        ),
        (json!(true), json!(false))
    );
    assert_eq!(
        (
            weekend["is_trading_holiday"].clone(),
            weekend["is_weekend"].clone()
        ),
        (json!(false), json!(true))
    );
    assert_eq!(holiday["is_special_session"], false);
    assert_eq!(holiday["is_trading_day"], false);
    // Muhurat trading: a Sunday that trades.
    let muhurat = t.describe(d(2026, 11, 8));
    assert_eq!(muhurat["is_trading_day"], true);
    assert_eq!(muhurat["is_special_session"], true);
    assert_eq!(muhurat["is_weekend"], true);
    assert_eq!(muhurat["is_trading_holiday"], false);
}

/// N-calendar-05: a settlement holiday (no exchange closed) is neither a
/// closure nor a special session; the web made a Saturday one a trading
/// day. Fails on the web's classification
/// (`kind == "SPECIAL_SESSION" or not closed_exchanges`).
#[test]
fn a_settlement_holiday_is_not_a_special_session() {
    let mut years = HashMap::new();
    years.insert(
        2026,
        Ok(vec![
            json!({"date": "2026-11-21", "holiday_type": "SETTLEMENT_HOLIDAY", "closed_exchanges": []}),
            json!({"date": "2026-11-24", "holiday_type": "SETTLEMENT_HOLIDAY", "closed_exchanges": []}),
        ]),
    );
    let cal = FakeCalendar(years);
    let t = TradingDays::new(&cal);
    let saturday = t.describe(d(2026, 11, 21));
    assert_eq!(saturday["is_trading_day"], false);
    assert_eq!(saturday["is_special_session"], false);
    let tuesday = t.describe(d(2026, 11, 24));
    assert_eq!(tuesday["is_trading_day"], true, "trading stays open");
    assert_eq!(tuesday["is_trading_holiday"], false);
}

#[test]
fn an_unreadable_year_is_weekends_only() {
    let mut years = HashMap::new();
    years.insert(2026, Err("unavailable".to_string()));
    let cal = FakeCalendar(years);
    let t = TradingDays::new(&cal);
    assert!(t.is_trading_day(d(2026, 1, 26)));
    assert!(!t.is_trading_day(d(2026, 1, 25)));
}

#[test]
fn dates_read_as_python_312_fromisoformat() {
    assert_eq!(from_isoformat("2026-10-29"), Some(d(2026, 10, 29)));
    assert_eq!(from_isoformat("20261029"), Some(d(2026, 10, 29)));
    assert_eq!(from_isoformat("2026-W44-5"), Some(d(2026, 10, 30)));
    assert_eq!(from_isoformat("2026W445"), Some(d(2026, 10, 30)));
    assert_eq!(from_isoformat("2026-W44"), Some(d(2026, 10, 26)));
    // The C parser does not look past what it reads ("20261029T0").
    assert_eq!(from_isoformat("20261029T0"), Some(d(2026, 10, 29)));
    for bad in [
        "31/12/2026",
        "2026-13-01",
        "2026-02-30",
        "2026-1-01",
        "0000-01-01",
        "",
        "2026-10",
    ] {
        assert_eq!(from_isoformat(bad), None, "{}", bad);
    }
}

/// N-dataCommon-10, N-calendar-02: a blank date is the IST trading session
/// date on any machine clock; 02:30 IST on Monday is still Sunday's
/// session (boundary 03:00).
#[tokio::test]
async fn a_blank_date_is_the_ist_session_date() {
    let env = env_with(FakeBook(Ok(Vec::new())), FakeCalendar::default());
    let ctx = ctx_at(
        Arc::new(FakeFlowClient::new()),
        "2026-11-02T02:30:00",
        "w",
        env,
    );
    let out = run(&ctx, "calendar", json!({"outputVariable": "cal"})).await;
    let v = out.value().cloned().unwrap_or(Value::Null);
    assert_eq!(v["date"], "2026-11-01");
    assert_eq!(v["weekday"], "Sunday");
    assert_eq!(
        logs(&ctx)[0].1,
        "Calendar 2026-11-01 (Sunday): trading=False newWeek=False newMonth=False newQuarter=False"
    );
    assert_eq!(ctx.vars.get("cal"), Some(v));
}

#[tokio::test]
async fn a_bad_date_fails_without_storing() {
    let ctx = ctx_at(
        Arc::new(FakeFlowClient::new()),
        "2026-10-29T10:15:00",
        "w",
        env_with(FakeBook(Ok(Vec::new())), FakeCalendar::default()),
    );
    let out = run(
        &ctx,
        "calendar",
        json!({"date": "31/12/2026", "outputVariable": "cal"}),
    )
    .await;
    assert_eq!(
        out.value().cloned(),
        Some(
            json!({"status": "error", "message": "Calendar date must be YYYY-MM-DD, got '31/12/2026'"})
        )
    );
    assert_eq!(ctx.vars.get("cal"), None);
}

/// N-calendar-04, decision 21: holidays are read fresh on every run (no
/// 24-hour cache), so an edit on the Holidays page applies to the next run.
#[test]
fn every_run_reads_the_calendar_afresh() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting(AtomicUsize);
    impl crate::flow::contract::env::TradingCalendar for Counting {
        fn holidays(&self, _: i32) -> Result<Vec<Value>, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }
    let source = Counting(AtomicUsize::new(0));
    TradingDays::new(&source).describe(d(2026, 8, 3));
    let first = source.0.load(Ordering::SeqCst);
    assert!(first >= 1);
    TradingDays::new(&source).describe(d(2026, 8, 3));
    assert_eq!(
        source.0.load(Ordering::SeqCst),
        2 * first,
        "the second run reads again"
    );
}

/// N-calendar-04, decision 32: a calendar the app cannot read degrades to
/// weekends only and logs the web's warning, at warning level, whatever the
/// cause (the web's service catches its failure and answers success=False).
#[test]
fn a_failed_calendar_read_logs_the_webs_unavailable_warning() {
    for cause in [
        crate::flow::contract::env::UNAVAILABLE.to_string(),
        "The market calendar could not be read: no such table".to_string(),
    ] {
        let mut years = HashMap::new();
        years.insert(2026, Err(cause.clone()));
        let cal = FakeCalendar(years);
        let log = ServerLog::capture();
        let trading = TradingDays::new(&cal).is_trading_day(d(2026, 1, 26));
        assert!(trading, "weekends only: a Monday trades");
        assert_eq!(
            log.lines(),
            vec![(
                "WARN".to_string(),
                "Holiday list for 2026 unavailable; assuming weekends only".to_string()
            )]
        );
    }
}

/// N-calendar-04: the previous and next trading day are searched at most
/// 30 days out, as the web bounds its loop; past a longer closure the day
/// the search stopped on is returned (31 days away), not the open day
/// beyond it.
#[test]
fn the_trading_day_search_stops_after_thirty_days() {
    let first = d(2026, 2, 1);
    let closed: Vec<Value> = (0..45)
        .map(|i| {
            let day = first + chrono::Duration::days(i);
            json!({"date": day.format("%Y-%m-%d").to_string(), "holiday_type": "TRADING_HOLIDAY", "closed_exchanges": ["NSE"]})
        })
        .collect();
    let mut years = HashMap::new();
    years.insert(2026, Ok(closed));
    let cal = FakeCalendar(years);
    let t = TradingDays::new(&cal);
    // Closed 1 Feb .. 17 Mar 2026; 30 Jan and 18 Mar trade.
    assert!(t.is_trading_day(d(2026, 1, 30)) && t.is_trading_day(d(2026, 3, 18)));
    assert_eq!(t.prev_trading_day(d(2026, 3, 18)), d(2026, 2, 15));
    assert_eq!(t.next_trading_day(d(2026, 1, 31)), d(2026, 3, 3));
    // Within the bound the search reaches the open day.
    assert_eq!(t.prev_trading_day(d(2026, 3, 19)), d(2026, 3, 18));
    assert_eq!(t.next_trading_day(d(2026, 3, 10)), d(2026, 3, 18));
}
