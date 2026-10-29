//! `test/test_flow_market_hours.py`, ported: the window comes from the
//! calendar, a closed day stays closed, the node's own times narrow or
//! extend it, garbage never widens it (parity X-112..X-126, T-020, T-021).

use super::*;
use serde_json::json;
use std::cell::RefCell;

const NSE: (u32, u32) = (9 * 60 + 15, 15 * 60 + 30);
const MCX: (u32, u32) = (9 * 60, 23 * 60 + 55);

fn at(h: u32, m: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 3)
        .unwrap()
        .and_hms_opt(h, m, 0)
        .unwrap()
}

fn hours(start: Option<&str>, end: Option<&str>, exchange: Option<&str>) -> MarketHours {
    MarketHours {
        enabled: true,
        start: start.map(String::from),
        end: end.map(String::from),
        exchange: exchange.map(String::from),
    }
}

fn open(w: (u32, u32)) -> impl Fn(NaiveDate, &str) -> Window {
    move |_, _| Some(w)
}

fn closed(_: NaiveDate, _: &str) -> Window {
    None
}

/// X-112: the equity session, both ends inclusive.
#[test]
fn equity_session() {
    for (h, m, want) in [
        (9, 14, false),
        (9, 15, true),
        (15, 30, true),
        (15, 31, false),
        (3, 0, false),
    ] {
        assert_eq!(
            is_within(at(h, m), &hours(None, None, None), open(NSE)),
            want,
            "{}:{}",
            h,
            m
        );
    }
}

/// X-113: MCX's evening is open on MCX's own window.
#[test]
fn mcx_evening_is_open_where_the_old_fixed_window_shut_it() {
    let by_exchange = |_: NaiveDate, ex: &str| Some(if ex == "MCX" { MCX } else { NSE });
    assert!(is_within(
        at(22, 30),
        &hours(None, None, Some("MCX")),
        by_exchange
    ));
    assert!(!is_within(
        at(22, 30),
        &hours(None, None, Some("NSE")),
        by_exchange
    ));
}

/// X-114, X-115: the exchange reaches the calendar upper-cased, NSE when
/// none is named.
#[test]
fn the_exchange_is_passed_to_the_calendar() {
    for (given, want) in [(Some("mcx"), "MCX"), (None, "NSE")] {
        let asked = RefCell::new(String::new());
        is_within(at(12, 0), &hours(None, None, given), |_, ex| {
            *asked.borrow_mut() = ex.to_string();
            Some(NSE)
        });
        assert_eq!(*asked.borrow(), want);
    }
}

/// X-116, X-117: a closed day blocks the run, and an override cannot
/// reopen it.
#[test]
fn a_closed_day_stays_closed() {
    assert!(!is_within(at(12, 0), &hours(None, None, None), closed));
    assert!(!is_within(
        at(12, 0),
        &hours(Some("00:00"), Some("23:59"), None),
        closed
    ));
}

/// X-118..X-121: the node's times extend or narrow the session; one side
/// alone leaves the other to the calendar; the ends are inclusive.
#[test]
fn the_nodes_own_times_narrow_or_extend_the_session() {
    assert!(!is_within(at(15, 40), &hours(None, None, None), open(NSE)));
    assert!(is_within(
        at(15, 40),
        &hours(None, Some("15:40"), None),
        open(NSE)
    ));
    assert!(is_within(at(9, 20), &hours(None, None, None), open(NSE)));
    assert!(!is_within(
        at(9, 20),
        &hours(Some("09:30"), None, None),
        open(NSE)
    ));
    let end_only = hours(None, Some("15:40"), None);
    assert!(!is_within(at(9, 14), &end_only, open(NSE)));
    assert!(is_within(at(15, 35), &end_only, open(NSE)));
    let both = hours(Some("10:00"), Some("14:00"), None);
    assert!(is_within(at(10, 0), &both, open(NSE)));
    assert!(is_within(at(14, 0), &both, open(NSE)));
    assert!(!is_within(at(14, 1), &both, open(NSE)));
}

/// X-122: a time that does not parse falls back to the calendar, never to
/// all day.
#[test]
fn a_malformed_override_falls_back_to_the_calendar() {
    for end in ["", "nonsense", "25:00", "12:99", "1530", "12:"] {
        let h = hours(None, Some(end), None);
        assert!(!is_within(at(23, 0), &h, open(NSE)), "{}", end);
        assert!(is_within(at(12, 0), &h, open(NSE)), "{}", end);
    }
    assert_eq!(parse_hhmm(Some("9:15")), Some(555));
    assert_eq!(parse_hhmm(Some(" 09:15 ")), Some(555));
    assert_eq!(parse_hhmm(None), None);
}

/// X-123..X-125: the settings come from the first start node; absent keys
/// stay absent; no start node is ungated.
#[test]
fn the_settings_are_read_from_the_start_node() {
    let nodes = json!([
        {"id": "s", "type": "start", "data": {"marketHoursOnly": true, "marketHoursStart": "09:15",
            "marketHoursEnd": "15:40", "marketHoursExchange": "NFO"}},
        {"id": "s2", "type": "start", "data": {"marketHoursOnly": false}}
    ]);
    assert_eq!(
        config(&nodes),
        hours(Some("09:15"), Some("15:40"), Some("NFO"))
    );
    let bare = json!([{"id": "s", "type": "start", "data": {"marketHoursOnly": true}}]);
    assert_eq!(config(&bare), hours(None, None, None));
    assert!(!config(&json!([{"id": "w", "type": "webhookTrigger", "data": {}}])).enabled);
    assert!(!config(&Value::Null).enabled);
    assert!(!config(&json!([{"id": "s", "type": "start"}])).enabled);
}

/// X-126 (`TestRealCalendar`, never skipped here): the desktop's bundled
/// holidays and timings, seeded in a temporary database.
#[test]
fn the_desktop_calendar_drives_the_window() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::sqlite::migrations::run_legacy_schema(&conn).unwrap();
    crate::db::sqlite::market_calendar::migrate(&conn).unwrap();
    let calendar = |date: NaiveDate, ex: &str| {
        let w = crate::db::sqlite::market_calendar::timings_for_date(&conn, date).unwrap();
        window_of(&w, ex)
    };
    let day = |d: u32, h: u32, m: u32| {
        NaiveDate::from_ymd_opt(2026, 10, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    };
    let none = hours(None, None, None);
    assert!(!is_within(day(31, 12, 0), &none, calendar), "a Saturday");
    assert!(is_within(day(27, 12, 0), &none, calendar), "a Tuesday");
    assert!(
        !is_within(day(20, 12, 0), &none, calendar),
        "a trading holiday"
    );
    assert!(!is_within(day(27, 15, 40), &none, calendar));
    assert!(is_within(
        day(27, 15, 40),
        &hours(None, Some("15:40"), None),
        calendar
    ));
}
