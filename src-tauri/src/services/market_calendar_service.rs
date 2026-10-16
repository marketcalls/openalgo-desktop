//! Market calendar service (web `services/market_calendar_service.py` and
//! the admin holiday / timing routes): response shapes for
//! `/api/v1/market/{holidays,timings}` and the admin pages.

use crate::db::sqlite::market_calendar::{self as cal, Holiday, OpenWindow};
use crate::error::Result;
use crate::sandbox::TradingDayFn;
use crate::state::AppState;
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc, Weekday};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::sync::{Arc, Weak};

/// Today's date in IST from the app clock.
pub fn today_ist(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&Kolkata).date_naive()
}

fn window_json(w: &OpenWindow) -> Value {
    json!({"exchange": w.exchange, "start_time": w.start_time, "end_time": w.end_time})
}

fn holiday_api_json(h: &Holiday) -> Value {
    json!({
        "date": h.date,
        "description": h.description,
        "holiday_type": h.holiday_type,
        "closed_exchanges": h.closed,
        "open_exchanges": h.open.iter().map(window_json).collect::<Vec<_>>(),
    })
}

/// `POST /api/v1/market/holidays` body (web `get_holidays`).
pub fn holidays_api(ctx: &AppState, year: Option<i32>) -> Result<Value> {
    let year = year.unwrap_or_else(|| today_ist(ctx.now()).year());
    let conn = ctx.sqlite.conn()?;
    let list = cal::holidays_by_year(&conn, year)?;
    Ok(json!({
        "status": "success",
        "year": year,
        "timezone": "Asia/Kolkata",
        "data": list.iter().map(holiday_api_json).collect::<Vec<_>>(),
    }))
}

/// Trading windows for a date, epoch ms (web `get_market_timings_for_date`).
pub fn timings_for(ctx: &AppState, date: NaiveDate) -> Result<Vec<OpenWindow>> {
    let conn = ctx.sqlite.conn()?;
    cal::timings_for_date(&conn, date)
}

/// The sandbox's trading days, for its dated P&L snapshot (web
/// `sandbox/catch_up_processor.is_pnl_snapshot_day` on an install whose
/// session expires daily): every day that is not a market holiday, so a
/// weekday holiday gets no snapshot and a weekend special session does. A
/// calendar that cannot be read falls back to weekdays, as the web's check
/// does. Holds the app weakly: the sandbox lives inside it.
pub fn sandbox_trading_day(app: Weak<AppState>) -> TradingDayFn {
    Arc::new(move |date: NaiveDate| {
        let weekday = !matches!(date.weekday(), Weekday::Sat | Weekday::Sun);
        let Some(ctx) = app.upgrade() else {
            return weekday;
        };
        let holiday = ctx
            .sqlite
            .conn()
            .and_then(|conn| cal::is_market_holiday(&conn, date));
        match holiday {
            Ok(h) => !h,
            Err(e) => {
                tracing::error!(
                    "Could not read the market calendar for the sandbox P&L snapshot of {}: {}",
                    date,
                    e
                );
                weekday
            }
        }
    })
}

pub fn timings_api_json(list: &[OpenWindow]) -> Value {
    Value::Array(list.iter().map(window_json).collect())
}

/// Epoch ms windows as IST `HH:MM` (the admin page's display rows).
pub fn windows_hhmm(list: &[OpenWindow]) -> Vec<Value> {
    let fmt = |ms: i64| {
        Kolkata
            .timestamp_millis_opt(ms)
            .single()
            .map(|d| d.format("%H:%M").to_string())
            .unwrap_or_default()
    };
    list.iter()
        .map(|w| {
            json!({
                "exchange": w.exchange,
                "start_time": fmt(w.start_time),
                "end_time": fmt(w.end_time),
            })
        })
        .collect()
}

/// Supported date range for the API (web `_is_supported_date`).
pub fn supported_date(d: NaiveDate) -> bool {
    d >= NaiveDate::from_ymd_opt(2020, 1, 1).unwrap_or_default()
        && d <= NaiveDate::from_ymd_opt(2050, 12, 31).unwrap_or_default()
}

/// `GET /admin/api/holidays` body.
pub fn admin_holidays(ctx: &AppState, year: Option<i32>) -> Result<Value> {
    let current = today_ist(ctx.now()).year();
    let year = year.unwrap_or(current);
    let conn = ctx.sqlite.conn()?;
    let list = cal::holidays_by_year(&conn, year)?;
    let mut years = cal::holiday_years(&conn)?;
    drop(conn);
    if years.is_empty() {
        years.push(current);
    }
    for y in [current, current + 1] {
        if !years.contains(&y) {
            years.push(y);
        }
    }
    years.sort_unstable();
    let data: Vec<Value> = list
        .iter()
        .map(|h| {
            let day = NaiveDate::parse_from_str(&h.date, "%Y-%m-%d")
                .map(|d| d.format("%A").to_string())
                .unwrap_or_default();
            json!({
                "id": h.id,
                "date": h.date,
                "day_name": day,
                "description": h.description,
                "holiday_type": h.holiday_type,
                "closed_exchanges": h.closed,
            })
        })
        .collect();
    Ok(json!({
        "status": "success",
        "data": data,
        "current_year": year,
        "years": years,
        "exchanges": cal::SUPPORTED_EXCHANGES,
    }))
}

/// How many holidays are listed.
pub fn holiday_count(ctx: &AppState) -> Result<i64> {
    let conn = ctx.sqlite.conn()?;
    cal::holiday_count(&conn)
}

/// List a holiday. `None`: one is already listed on that date.
pub fn add_holiday(ctx: &AppState, new: &cal::NewHoliday) -> Result<Option<i64>> {
    let conn = ctx.sqlite.conn()?;
    cal::add_holiday(&conn, new)
}

/// Remove the holiday `id`, answering its description. `None`: no such
/// holiday.
pub fn delete_holiday(ctx: &AppState, id: i64) -> Result<Option<String>> {
    let conn = ctx.sqlite.conn()?;
    cal::delete_holiday(&conn, id)
}

/// Set an exchange's regular trading hours (`HH:MM`, IST).
pub fn update_timing(ctx: &AppState, exchange: &str, start: &str, end: &str) -> Result<()> {
    let conn = ctx.sqlite.conn()?;
    cal::update_timing(&conn, exchange, start, end)
}

/// `GET /admin/api/timings` body.
pub fn admin_timings(ctx: &AppState) -> Result<Value> {
    let today = today_ist(ctx.now());
    let conn = ctx.sqlite.conn()?;
    let all = cal::all_timings(&conn)?;
    let today_windows = cal::timings_for_date(&conn, today)?;
    drop(conn);
    let data: Vec<Value> = all
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "exchange": t.exchange,
                "start_time": t.start_time,
                "end_time": t.end_time,
                "start_offset": t.start_offset,
                "end_offset": t.end_offset,
            })
        })
        .collect();
    Ok(json!({
        "status": "success",
        "data": data,
        "market_status": timings_api_json(&today_windows),
        "today_timings": windows_hhmm(&today_windows),
        "today": today.format("%Y-%m-%d").to_string(),
        "exchanges": cal::SUPPORTED_EXCHANGES,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ist_formatting_of_windows() {
        let w = OpenWindow {
            exchange: "NSE".into(),
            start_time: 1_790_826_300_000,
            end_time: 1_790_848_800_000,
        };
        let v = windows_hhmm(&[w]);
        assert_eq!(v[0]["start_time"], "09:15");
        assert_eq!(v[0]["end_time"], "15:30");
        assert!(supported_date(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()));
        assert!(!supported_date(
            NaiveDate::from_ymd_opt(2019, 12, 31).unwrap()
        ));
    }
}
