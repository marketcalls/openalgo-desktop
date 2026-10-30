//! The scheduler's market-hours gate (the web's `get_market_hours_config`,
//! `_parse_hhmm` and `is_within_market_hours`,
//! `flow_scheduler_service.py` 342-429, 532-552).
//!
//! The window comes from the desktop's trading calendar
//! (`services::market_calendar_service::timings_for`: special sessions,
//! holidays with an open window for some exchanges, weekends, the
//! configured hours), for the exchange the start node names (NSE when it
//! names none). A closed day stays closed whatever the node's own times
//! say; those only narrow or extend the clock on an open day. Only
//! scheduled runs are gated: webhook, price-alert, order-update and Run Now
//! runs never are (parity T-022).

use chrono::{NaiveDate, NaiveDateTime, Timelike};
use serde_json::Value;

/// The calendar consulted when the start node names none.
pub const DEFAULT_EXCHANGE: &str = "NSE";

/// The start node's market-hours settings, read from the graph on every
/// fire, so an edit applies from the next run without reactivation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarketHours {
    pub enabled: bool,
    pub start: Option<String>,
    pub end: Option<String>,
    pub exchange: Option<String>,
}

/// `value or None` for a node field: a truthy value as text.
fn text(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .filter(|v| crate::flow::contract::pyfmt::truthy(v))
        .map(crate::flow::contract::pyfmt::str)
}

/// From the first `start` node (`get_market_hours_config`); no start node
/// is ungated.
pub fn config(nodes: &Value) -> MarketHours {
    let Some(start) = nodes
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n.get("type").and_then(Value::as_str) == Some("start"))
    else {
        return MarketHours::default();
    };
    let data = start.get("data").cloned().unwrap_or(Value::Null);
    MarketHours {
        enabled: data
            .get("marketHoursOnly")
            .is_some_and(crate::flow::contract::pyfmt::truthy),
        start: text(&data, "marketHoursStart"),
        end: text(&data, "marketHoursEnd"),
        exchange: text(&data, "marketHoursExchange"),
    }
}

/// `"H:MM"` / `"HH:MM"` as minutes past midnight; anything else `None`, so
/// the calendar decides instead of a time nobody configured (`_parse_hhmm`).
pub fn parse_hhmm(value: Option<&str>) -> Option<u32> {
    let (h, m) = value?.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h <= 23 && m <= 59).then_some(h * 60 + m)
}

/// The exchange's open window on a date, as IST minutes past midnight
/// `(start, end)`; `None` when it is shut that day.
pub type Window = Option<(u32, u32)>;

/// Whether `now` (IST wall time) is inside the window (`is_within_market_hours`):
/// `calendar(date, exchange)` gives the day's window for the upper-cased
/// exchange; a node time that does not parse falls back to the calendar's.
/// Both ends are inclusive, at minute resolution.
pub fn is_within(
    now: NaiveDateTime,
    hours: &MarketHours,
    calendar: impl Fn(NaiveDate, &str) -> Window,
) -> bool {
    let exchange = hours
        .exchange
        .as_deref()
        .unwrap_or(DEFAULT_EXCHANGE)
        .to_uppercase();
    let Some((open, close)) = calendar(now.date(), &exchange) else {
        return false;
    };
    let start = parse_hhmm(hours.start.as_deref()).unwrap_or(open);
    let end = parse_hhmm(hours.end.as_deref()).unwrap_or(close);
    let minutes = now.hour() * 60 + now.minute();
    start <= minutes && minutes <= end
}

/// The desktop calendar's window for `exchange` on `date`, from the
/// windows `timings_for` lists (epoch milliseconds).
pub fn window_of(
    windows: &[crate::db::sqlite::market_calendar::OpenWindow],
    exchange: &str,
) -> Window {
    use chrono::TimeZone;
    use chrono_tz::Asia::Kolkata;
    let w = windows.iter().find(|w| w.exchange == exchange)?;
    let minutes = |ms: i64| {
        Kolkata
            .timestamp_millis_opt(ms)
            .single()
            .map(|t| t.hour() * 60 + t.minute())
    };
    Some((minutes(w.start_time)?, minutes(w.end_time)?))
}

#[cfg(test)]
mod tests;
