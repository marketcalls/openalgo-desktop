//! IST time helpers. The web converts with the server's local zone, which
//! for its users is IST; the desktop pins IST so results do not depend on
//! the machine's zone.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;

/// IST is UTC+05:30.
pub const IST_OFFSET: i64 = 19_800;

/// Wall-clock IST of an instant, to the second (how timestamps are stored).
pub fn ist_naive(t: DateTime<Utc>) -> NaiveDateTime {
    use chrono::Timelike;
    let n = t.with_timezone(&Kolkata).naive_local();
    n.with_nanosecond(0).unwrap_or(n)
}

/// The instant of an IST wall-clock time.
pub fn from_ist(n: NaiveDateTime) -> DateTime<Utc> {
    match Kolkata.from_local_datetime(&n).single() {
        Some(t) => t.with_timezone(&Utc),
        None => Utc.from_utc_datetime(&(n - Duration::seconds(IST_OFFSET))),
    }
}

/// Epoch seconds of IST midnight on `d`.
pub fn day_start(d: NaiveDate) -> i64 {
    d.and_hms_opt(0, 0, 0)
        .map(|n| n.and_utc().timestamp() - IST_OFFSET)
        .unwrap_or(0)
}

/// IST calendar date of an epoch (web `datetime.fromtimestamp(ts).date()`).
pub fn ist_day(ts: i64) -> NaiveDate {
    DateTime::from_timestamp(ts + IST_OFFSET, 0)
        .map(|d| d.date_naive())
        .unwrap_or_default()
}

/// `YYYY-MM-DD` of an epoch in IST.
pub fn ist_date(ts: i64) -> String {
    ist_day(ts).format("%Y-%m-%d").to_string()
}

/// `(date, time)` strings of an epoch shifted by IST.
pub fn ist_date_time(ts: i64) -> (String, String) {
    match DateTime::from_timestamp(ts + IST_OFFSET, 0) {
        Some(d) => (
            d.format("%Y-%m-%d").to_string(),
            d.format("%H:%M:%S").to_string(),
        ),
        None => (String::new(), String::new()),
    }
}

/// `YYYY-MM-DD`, strictly (web `strptime(.., "%Y-%m-%d")`).
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// Flask's rendering of a raw datetime (`http_date`), from a stored
/// `YYYY-MM-DDTHH:MM:SS`.
pub fn http_date(iso: &str) -> Option<String> {
    let n = NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S").ok()?;
    Some(n.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
}

/// ISO 8601 with the IST offset (APScheduler's `next_run_time.isoformat()`).
pub fn iso_ist(t: DateTime<Utc>) -> String {
    t.with_timezone(&Kolkata)
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ist_conversions() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        assert_eq!(day_start(d), 1_704_047_400);
        assert_eq!(ist_date(1_704_047_400), "2024-01-01");
        assert_eq!(ist_date(1_704_047_399), "2023-12-31");
        assert_eq!(
            ist_date_time(1_704_080_700),
            ("2024-01-01".into(), "09:15:00".into())
        );
        assert_eq!(
            http_date("2026-10-07T09:15:00").unwrap(),
            "Wed, 07 Oct 2026 09:15:00 GMT"
        );
        let t = from_ist(d.and_hms_opt(9, 15, 0).unwrap());
        assert_eq!(iso_ist(t), "2024-01-01T09:15:00+05:30");
        assert_eq!(ist_naive(t), d.and_hms_opt(9, 15, 0).unwrap());
    }
}
