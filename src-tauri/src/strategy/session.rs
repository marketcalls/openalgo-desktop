//! Where one trading session ends and the next begins (web
//! `services/strategy_module/session.py`).
//!
//! The platform ends its day at `SESSION_EXPIRY_TIME` (03:00 IST by
//! default), when broker tokens are revoked. That instant, not midnight, is
//! every "today" here: a signal run rolls at it and a daily loss limit resets
//! on it. 01:00 on Tuesday belongs to Monday's session.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Asia::Kolkata;

/// The IST session day a moment belongs to.
pub fn session_day(moment: DateTime<Utc>, hour: u32, minute: u32) -> NaiveDate {
    let ist = moment.with_timezone(&Kolkata);
    let reset = NaiveTime::from_hms_opt(hour.min(23), minute.min(59), 0).unwrap_or_default();
    if ist.time() < reset {
        ist.date_naive().pred_opt().unwrap_or(ist.date_naive())
    } else {
        ist.date_naive()
    }
}

/// The instant the session containing `now` began.
pub fn session_started_at(now: DateTime<Utc>, hour: u32, minute: u32) -> DateTime<Utc> {
    crate::session::boundary::last_boundary(now, hour, minute)
}

/// Whether `started` belongs to an earlier session than `now`.
pub fn started_before_today(
    started: DateTime<Utc>,
    now: DateTime<Utc>,
    hour: u32,
    minute: u32,
) -> bool {
    session_day(started, hour, minute) < session_day(now, hour, minute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ist(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Kolkata
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn before_the_reset_belongs_to_the_previous_day() {
        let tue_1am = ist(2026, 10, 6, 1, 0);
        let mon_10pm = ist(2026, 10, 5, 22, 0);
        assert_eq!(session_day(tue_1am, 3, 0), session_day(mon_10pm, 3, 0));
        assert!(started_before_today(
            mon_10pm,
            ist(2026, 10, 6, 9, 15),
            3,
            0
        ));
        assert!(!started_before_today(
            ist(2026, 10, 6, 3, 0),
            ist(2026, 10, 6, 9, 15),
            3,
            0
        ));
    }

    #[test]
    fn the_session_starts_at_the_boundary_in_ist() {
        let start = session_started_at(ist(2026, 10, 6, 9, 15), 3, 0);
        assert_eq!(start, ist(2026, 10, 6, 3, 0));
    }
}
