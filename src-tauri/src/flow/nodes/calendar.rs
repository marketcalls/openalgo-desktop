//! Trading-day arithmetic (a port of the web's `utils/trading_calendar.py`)
//! and the Calendar node (`execute_calendar`): "has a new week started" is
//! "today is the first trading day of this week", answerable from the
//! exchange calendar alone, with no memory between runs.
//!
//! Holidays come from the desktop's market calendar (the data behind
//! `/api/v1/market/holidays`), read once per year per run; an unreadable
//! year degrades to weekends only, as on the web. Not exchange-aware: any
//! trading holiday closes the day.
//!
//! One web defect is fixed (N-calendar-05): only a `SPECIAL_SESSION` entry
//! is a special session and only a `TRADING_HOLIDAY` closes the market. The
//! web also counted any entry without closed exchanges as a special session,
//! so a settlement holiday falling on a Saturday read as a trading day.

use crate::flow::contract::env::TradingCalendar;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome};
use crate::flow::contract::pyfmt;
use crate::state::AppState;
use async_trait::async_trait;
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

/// One year's holiday sets: dates closed, dates with a special session.
#[derive(Default)]
struct Year {
    closed: HashSet<NaiveDate>,
    special: HashSet<NaiveDate>,
}

/// Periods a workflow asks about.
#[derive(Clone, Copy, Debug)]
pub enum Period {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

/// The calendar for one question: each year read once, lazily.
pub struct TradingDays<'a> {
    source: &'a dyn TradingCalendar,
    years: RefCell<HashMap<i32, Year>>,
}

fn quarter_of(day: NaiveDate) -> u32 {
    (day.month() - 1) / 3 + 1
}

impl<'a> TradingDays<'a> {
    pub fn new(source: &'a dyn TradingCalendar) -> Self {
        Self {
            source,
            years: RefCell::default(),
        }
    }

    /// `_calendar_entries(year)`: the year's entries classified; nothing on
    /// a failed read (weekends only).
    fn load(&self, year: i32) -> Year {
        let entries = match self.source.holidays(year) {
            Ok(e) => e,
            // The web's market calendar service catches a failed read and
            // answers success=False, which the web logs as this warning
            // (its "Could not load holidays" line is for a service that
            // raises, which the desktop's never does).
            Err(e) => {
                tracing::warn!(
                    "Holiday list for {} unavailable; assuming weekends only",
                    year
                );
                tracing::debug!("Holiday read for {} failed: {}", year, e);
                return Year::default();
            }
        };
        let mut out = Year::default();
        for entry in entries {
            let Some(raw) = entry.get("date").filter(|v| pyfmt::truthy(v)) else {
                continue;
            };
            let text: String = pyfmt::str(raw).chars().take(10).collect();
            let Some(day) = from_isoformat(&text) else {
                continue;
            };
            let kind = entry
                .get("holiday_type")
                .filter(|v| pyfmt::truthy(v))
                .map(pyfmt::str)
                .unwrap_or_default()
                .to_uppercase();
            match kind.as_str() {
                "SPECIAL_SESSION" => {
                    out.special.insert(day);
                }
                "TRADING_HOLIDAY" => {
                    out.closed.insert(day);
                }
                _ => {}
            }
        }
        out
    }

    fn with_year<T>(&self, year: i32, f: impl FnOnce(&Year) -> T) -> T {
        let mut years = self.years.borrow_mut();
        let entry = years.entry(year).or_insert_with(|| self.load(year));
        f(entry)
    }

    fn is_holiday(&self, day: NaiveDate) -> bool {
        self.with_year(day.year(), |y| y.closed.contains(&day))
    }

    fn is_special(&self, day: NaiveDate) -> bool {
        self.with_year(day.year(), |y| y.special.contains(&day))
    }

    /// A special session trades even on a weekend; a holiday does not;
    /// otherwise Monday to Friday.
    pub fn is_trading_day(&self, day: NaiveDate) -> bool {
        if self.is_special(day) {
            return true;
        }
        if self.is_holiday(day) {
            return false;
        }
        day.weekday().num_days_from_monday() < 5
    }

    /// The most recent trading day strictly before `day`.
    pub fn prev_trading_day(&self, day: NaiveDate) -> NaiveDate {
        self.adjacent(day, -1)
    }

    /// The next trading day strictly after `day`.
    pub fn next_trading_day(&self, day: NaiveDate) -> NaiveDate {
        self.adjacent(day, 1)
    }

    /// The trading day before (or after, `step` 1) `day`, searching at most
    /// 30 days.
    fn adjacent(&self, day: NaiveDate, step: i64) -> NaiveDate {
        let mut cursor = day + Duration::days(step);
        for _ in 0..30 {
            if self.is_trading_day(cursor) {
                return cursor;
            }
            cursor += Duration::days(step);
        }
        cursor
    }

    fn first_of(&self, day: NaiveDate, p: Period) -> NaiveDate {
        let (start, end) = bounds(day, p);
        let mut cursor = start;
        while cursor <= end && !self.is_trading_day(cursor) {
            cursor += Duration::days(1);
        }
        cursor
    }

    fn last_of(&self, day: NaiveDate, p: Period) -> NaiveDate {
        let (start, end) = bounds(day, p);
        let mut cursor = end;
        while cursor >= start && !self.is_trading_day(cursor) {
            cursor -= Duration::days(1);
        }
        cursor
    }

    /// Whether a new period began on this trading day (false on a day the
    /// exchange is shut).
    pub fn is_first_of(&self, day: NaiveDate, p: Period) -> bool {
        self.is_trading_day(day) && day == self.first_of(day, p)
    }

    /// Whether this trading day closes the period.
    pub fn is_last_of(&self, day: NaiveDate, p: Period) -> bool {
        self.is_trading_day(day) && day == self.last_of(day, p)
    }

    /// `describe(day)`: everything a workflow branches on for one date.
    pub fn describe(&self, day: NaiveDate) -> Map<String, Value> {
        let iso = |d: NaiveDate| json!(d.format("%Y-%m-%d").to_string());
        let weekday = day.weekday();
        let fields = [
            ("date", iso(day)),
            ("is_trading_day", json!(self.is_trading_day(day))),
            ("is_trading_holiday", json!(self.is_holiday(day))),
            (
                "is_weekend",
                json!(matches!(weekday, Weekday::Sat | Weekday::Sun)),
            ),
            ("is_special_session", json!(self.is_special(day))),
            ("weekday", json!(day.format("%A").to_string())),
            ("weekday_num", json!(weekday.number_from_monday())),
            ("day", json!(day.day())),
            ("month", json!(day.month())),
            ("quarter", json!(quarter_of(day))),
            ("year", json!(day.year())),
            ("week_of_year", json!(day.iso_week().week())),
            ("day_of_year", json!(day.ordinal())),
            ("is_new_day", json!(self.is_first_of(day, Period::Day))),
            ("is_new_week", json!(self.is_first_of(day, Period::Week))),
            ("is_new_month", json!(self.is_first_of(day, Period::Month))),
            (
                "is_new_quarter",
                json!(self.is_first_of(day, Period::Quarter)),
            ),
            ("is_new_year", json!(self.is_first_of(day, Period::Year))),
            (
                "is_last_day_of_week",
                json!(self.is_last_of(day, Period::Week)),
            ),
            (
                "is_last_day_of_month",
                json!(self.is_last_of(day, Period::Month)),
            ),
            (
                "is_last_day_of_quarter",
                json!(self.is_last_of(day, Period::Quarter)),
            ),
            (
                "is_last_day_of_year",
                json!(self.is_last_of(day, Period::Year)),
            ),
            ("prev_trading_day", iso(self.prev_trading_day(day))),
            ("next_trading_day", iso(self.next_trading_day(day))),
            (
                "first_trading_day_of_week",
                iso(self.first_of(day, Period::Week)),
            ),
            (
                "first_trading_day_of_month",
                iso(self.first_of(day, Period::Month)),
            ),
            (
                "first_trading_day_of_quarter",
                iso(self.first_of(day, Period::Quarter)),
            ),
            (
                "last_trading_day_of_week",
                iso(self.last_of(day, Period::Week)),
            ),
            (
                "last_trading_day_of_month",
                iso(self.last_of(day, Period::Month)),
            ),
            (
                "last_trading_day_of_quarter",
                iso(self.last_of(day, Period::Quarter)),
            ),
        ];
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }
}

/// The first and last calendar day of the period holding `day`; weeks run
/// Monday to Sunday.
fn bounds(day: NaiveDate, p: Period) -> (NaiveDate, NaiveDate) {
    let first_of_month =
        |d: NaiveDate, m: u32| NaiveDate::from_ymd_opt(d.year(), m, 1).unwrap_or(d);
    let month_end = |start: NaiveDate| {
        let next = start + Duration::days(32);
        next.with_day(1).unwrap_or(next) - Duration::days(1)
    };
    match p {
        Period::Day => (day, day),
        Period::Week => {
            let monday = day - Duration::days(i64::from(day.weekday().num_days_from_monday()));
            (monday, monday + Duration::days(6))
        }
        Period::Month => {
            let start = first_of_month(day, day.month());
            (start, month_end(start))
        }
        Period::Quarter => {
            let q = quarter_of(day);
            let start = first_of_month(day, 3 * (q - 1) + 1);
            (start, month_end(first_of_month(day, 3 * q)))
        }
        Period::Year => (
            first_of_month(day, 1),
            NaiveDate::from_ymd_opt(day.year(), 12, 31).unwrap_or(day),
        ),
    }
}

/// Python 3.12's `date.fromisoformat` (the C parser): `YYYY-MM-DD`,
/// `YYYYMMDD`, and ISO week dates `YYYY-Www[-D]` / `YYYYWww[D]`; lengths
/// 7, 8 or 10 only, as the web reads `raw[:10]`.
pub fn from_isoformat(text: &str) -> Option<NaiveDate> {
    let b = text.as_bytes();
    if !matches!(b.len(), 7 | 8 | 10) {
        return None;
    }
    let digits = |from: usize, n: usize| -> Option<u32> {
        let part = b.get(from..from + n)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let year = digits(0, 4).filter(|y| *y >= 1)?;
    let sep = b.get(4) == Some(&b'-');
    let mut p = 4 + usize::from(sep);
    if b.get(p) == Some(&b'W') {
        p += 1;
        let week = digits(p, 2)?;
        p += 2;
        let day = if p < b.len() {
            if sep {
                if b.get(p) != Some(&b'-') {
                    return None;
                }
                p += 1;
            }
            digits(p, 1)?
        } else {
            1
        };
        let weekday = Weekday::try_from(u8::try_from(day.checked_sub(1)?).ok()?).ok()?;
        return NaiveDate::from_isoywd_opt(i32::try_from(year).ok()?, week, weekday);
    }
    let month = digits(p, 2)?;
    p += 2;
    if sep {
        if b.get(p) != Some(&b'-') {
            return None;
        }
        p += 1;
    }
    let day = digits(p, 2)?;
    NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)
}

/// Calendar: trading-day facts for a date (blank is the trading session's
/// date, which turns at the session boundary, 03:00 IST unless configured).
pub struct CalendarNode;

#[async_trait]
impl FlowNode for CalendarNode {
    fn node_type(&self) -> &'static str {
        "calendar"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let raw = ctx.get_str(data, "date", "");
        let day = if raw.is_empty() {
            ctx.vars
                .builtin("session_date")
                .and_then(|d| from_isoformat(&d))
        } else {
            from_isoformat(&raw.chars().take(10).collect::<String>())
        };
        let Some(day) = day else {
            let message = format!(
                "Calendar date must be YYYY-MM-DD, got {}",
                pyfmt::quote(&raw)
            );
            ctx.error(format!("Calendar failed: {}", message));
            return NodeOutcome::error(message);
        };
        let source = ctx.env.calendar.clone();
        let described =
            tokio::task::spawn_blocking(move || TradingDays::new(source.as_ref()).describe(day))
                .await
                .unwrap_or_else(|e| {
                    tracing::error!("The trading calendar could not be read: {}", e);
                    TradingDays::new(&NoHolidays).describe(day)
                });
        let flag = |k: &str| pyfmt::str(described.get(k).unwrap_or(&Value::Null));
        ctx.info(format!(
            "Calendar {} ({}): trading={} newWeek={} newMonth={} newQuarter={}",
            flag("date"),
            flag("weekday"),
            flag("is_trading_day"),
            flag("is_new_week"),
            flag("is_new_month"),
            flag("is_new_quarter")
        ));
        let mut result = Map::new();
        result.insert("status".into(), json!("success"));
        result.extend(described);
        let result = Value::Object(result);
        ctx.store_output(data, result.clone());
        NodeOutcome::result(result)
    }
}

/// Weekends only.
struct NoHolidays;

impl TradingCalendar for NoHolidays {
    fn holidays(&self, _: i32) -> Result<Vec<Value>, String> {
        Ok(Vec::new())
    }
}

/// The app's market calendar, as the Calendar node reads it.
pub struct AppCalendar(pub Weak<AppState>);

impl TradingCalendar for AppCalendar {
    fn holidays(&self, year: i32) -> Result<Vec<Value>, String> {
        let app: Arc<AppState> = self.0.upgrade().ok_or("OpenAlgo is shutting down.")?;
        let body = crate::services::market_calendar_service::holidays_api(&app, Some(year))
            .map_err(|e| format!("The market calendar could not be read: {}", e))?;
        Ok(match body.get("data") {
            Some(Value::Array(list)) => list.clone(),
            _ => Vec::new(),
        })
    }
}
