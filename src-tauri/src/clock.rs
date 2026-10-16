//! Injected clock. Session expiry, OAuth state expiry and anything else that
//! depends on wall time reads it from here so tests are deterministic.

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveTime, Offset, TimeZone, Utc};
use parking_lot::RwLock;
use std::sync::Arc;

/// India Standard Time is UTC+05:30 all year (no daylight saving), so this
/// one offset converts every exchange time: the app's single copy of it.
pub const IST_OFFSET_SECS: i64 = 5 * 3600 + 30 * 60;

/// India Standard Time as a chrono offset.
pub fn ist_offset() -> FixedOffset {
    FixedOffset::east_opt(IST_OFFSET_SECS as i32).unwrap_or(Utc.fix())
}

/// Every instant the IST time of day `at` fell in `(from, to]` on the dates
/// `on` accepts, with its IST date, oldest first; never further back than
/// two days. A scheduler that ticked at `from` and ticks again at `to` passed
/// over these (the computer slept, or the runtime was held up), so it acts
/// on them or says it skipped them, rather than missing them silently.
pub fn ist_slots_between(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    at: NaiveTime,
    on: impl Fn(NaiveDate) -> bool,
) -> Vec<(NaiveDate, DateTime<Utc>)> {
    if to <= from {
        return Vec::new();
    }
    let from = from.max(to - Duration::days(2));
    let ist = ist_offset();
    let mut out = Vec::new();
    let mut day = from.with_timezone(&ist).date_naive();
    let last = to.with_timezone(&ist).date_naive();
    while day <= last {
        if on(day) {
            if let Some(t) = ist.from_local_datetime(&day.and_time(at)).single() {
                let t = t.with_timezone(&Utc);
                if t > from && t <= to {
                    out.push((day, t));
                }
            }
        }
        match day.succ_opt() {
            Some(next) => day = next,
            None => break,
        }
    }
    out
}

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Settable clock for tests.
#[derive(Debug)]
pub struct ManualClock {
    now: RwLock<DateTime<Utc>>,
}

impl ManualClock {
    pub fn new(now: DateTime<Utc>) -> Arc<Self> {
        Arc::new(Self {
            now: RwLock::new(now),
        })
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.write() = now;
    }

    pub fn advance(&self, by: chrono::Duration) {
        let mut n = self.now.write();
        *n += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn ist(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        ist_offset()
            .with_ymd_and_hms(2026, 10, d, h, m, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn slots_passed_over_are_found_once_and_never_more_than_two_days_back() {
        let at = NaiveTime::from_hms_opt(15, 15, 0).unwrap();
        let any = |_: NaiveDate| true;
        // A sleep from 15:00 to 16:30: the 15:15 slot, once.
        assert_eq!(
            ist_slots_between(ist(12, 15, 0), ist(12, 16, 30), at, any),
            vec![(
                NaiveDate::from_ymd_opt(2026, 10, 12).unwrap(),
                ist(12, 15, 15)
            )]
        );
        // The slot itself is in the span that ends on it, not the next.
        assert_eq!(
            ist_slots_between(ist(12, 15, 0), ist(12, 15, 15), at, any).len(),
            1
        );
        assert!(ist_slots_between(ist(12, 15, 15), ist(12, 15, 16), at, any).is_empty());
        // Across midnight, and only on accepted days.
        let monday = |d: NaiveDate| d.weekday() == chrono::Weekday::Mon;
        assert_eq!(
            ist_slots_between(ist(12, 15, 0), ist(13, 16, 0), at, monday),
            vec![(
                NaiveDate::from_ymd_opt(2026, 10, 12).unwrap(),
                ist(12, 15, 15)
            )]
        );
        // A week away looks back two days only; a clock that went back finds nothing.
        assert_eq!(
            ist_slots_between(ist(1, 9, 0), ist(8, 16, 0), at, any).len(),
            2
        );
        assert!(ist_slots_between(ist(12, 16, 0), ist(12, 15, 0), at, any).is_empty());
    }
}
