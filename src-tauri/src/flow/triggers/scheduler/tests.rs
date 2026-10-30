//! The schedule types against the web's `add_workflow_job` and APScheduler
//! (parity T-001..T-016), in IST whatever the computer's zone (T-014,
//! X-347: no part of this reads the local zone; CI runs it under UTC, and
//! the lane ran it under TZ=America/New_York).

use super::*;
use serde_json::json;

/// An IST wall time as an instant.
fn ist_at(d: u32, h: u32, m: u32, s: u32) -> DateTime<Utc> {
    from_ist(
        NaiveDate::from_ymd_opt(2026, 11, d)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap(),
    )
}

fn data(v: serde_json::Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn schedule(v: serde_json::Value, now: DateTime<Utc>) -> Result<Option<Schedule>, String> {
    Schedule::from_node(&data(v), now)
}

/// T-004: manual or absent registers nothing; an unknown type, `once`
/// without a date and `weekly` without days are refused with the web's
/// sentence.
#[test]
fn only_a_real_schedule_registers_a_job() {
    let now = ist_at(3, 10, 0, 0);
    assert_eq!(schedule(json!({}), now), Ok(None));
    assert_eq!(schedule(json!({"scheduleType": "manual"}), now), Ok(None));
    assert_eq!(schedule(json!({"scheduleType": ""}), now), Ok(None));
    for (v, kind) in [
        (json!({"scheduleType": "yearly"}), "yearly"),
        (json!({"scheduleType": "once"}), "once"),
        (json!({"scheduleType": "weekly", "days": []}), "weekly"),
    ] {
        assert_eq!(
            schedule(v, now),
            Err(format!("Invalid schedule configuration: type={}", kind))
        );
    }
    assert_eq!(job_id(42), "flow_workflow_42");
}

/// T-006, T-007: an interval fires on the IST clock's boundary plus 2 s,
/// stepped past now; hours on the top of the IST hour; seconds unaligned.
#[test]
fn an_interval_is_aligned_to_the_ist_clock() {
    let now = ist_at(3, 10, 7, 31);
    let first = |v: serde_json::Value| match schedule(v, now).unwrap().unwrap() {
        Schedule::Interval { start, every } => (start, every),
        other => panic!("{:?}", other),
    };
    assert_eq!(
        first(json!({"scheduleType": "interval", "intervalValue": 5, "intervalUnit": "minutes"})),
        (ist_at(3, 10, 10, 2), Duration::minutes(5))
    );
    assert_eq!(
        first(json!({"scheduleType": "interval", "intervalValue": 2, "intervalUnit": "hours"})),
        (ist_at(3, 12, 0, 2), Duration::hours(2))
    );
    assert_eq!(
        first(json!({"scheduleType": "interval", "intervalValue": 10, "intervalUnit": "seconds"})),
        (now + Duration::seconds(1), Duration::seconds(10))
    );
    // Value and unit default to 1 minute; an unknown unit is minutes.
    assert_eq!(
        first(json!({"scheduleType": "interval", "intervalUnit": "fortnights"})),
        (ist_at(3, 10, 8, 2), Duration::minutes(1))
    );
    let s = schedule(json!({"scheduleType": "interval", "intervalValue": 5}), now)
        .unwrap()
        .unwrap();
    assert_eq!(
        s.next_at_or_after(ist_at(3, 10, 10, 3)),
        Some(ist_at(3, 10, 15, 2))
    );
    assert!(schedule(
        json!({"scheduleType": "interval", "intervalValue": -5}),
        now
    )
    .is_err());
}

/// T-008, T-009 (WEB-DEFECT): a date alone runs at the node's Time in IST
/// (the web read it as midnight, which the market-hours gate skipped); a
/// full datetime is used as given; a time already past is refused.
#[test]
fn once_runs_at_its_date_and_time_in_ist() {
    let now = ist_at(3, 8, 0, 0);
    let at = |v: serde_json::Value| match schedule(v, now) {
        Ok(Some(Schedule::Once { at })) => Ok(at),
        Ok(other) => panic!("{:?}", other),
        Err(e) => Err(e),
    };
    assert_eq!(
        at(json!({"scheduleType": "once", "executeAt": "2026-11-03", "time": "10:30"})),
        Ok(ist_at(3, 10, 30, 0))
    );
    assert_eq!(
        at(json!({"scheduleType": "once", "executeAt": "2026-11-03"})),
        Ok(ist_at(3, 9, 15, 0)),
        "the default time, never midnight"
    );
    assert_eq!(
        at(json!({"scheduleType": "once", "executeAt": "2026-11-03T04:00:00Z"})),
        Ok(ist_at(3, 9, 30, 0))
    );
    assert_eq!(
        at(json!({"scheduleType": "once", "executeAt": "2026-11-03T11:45:00"})),
        Ok(ist_at(3, 11, 45, 0)),
        "a naive datetime is IST"
    );
    assert_eq!(
        at(json!({"scheduleType": "once", "executeAt": "soon"})),
        Err("Invalid datetime format: soon".into())
    );
    let past = at(json!({"scheduleType": "once", "executeAt": "2026-11-02", "time": "10:00"}));
    assert!(past.unwrap_err().contains("has already passed"));
}

/// T-011, T-012 (WEB-DEFECT): daily at its IST time, on its days when it
/// has them (the web's daily fired at weekends too); every day without.
#[test]
fn daily_honours_its_days() {
    // 2026-11-06 is a Friday.
    let now = ist_at(6, 10, 0, 0);
    let weekdays = schedule(
        json!({"scheduleType": "daily", "time": "09:15", "days": [0, 1, 2, 3, 4]}),
        now,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        weekdays.next_at_or_after(now),
        Some(ist_at(9, 9, 15, 0)),
        "not Saturday or Sunday"
    );
    let every_day = schedule(json!({"scheduleType": "daily", "time": "9:15"}), now)
        .unwrap()
        .unwrap();
    assert_eq!(every_day.next_at_or_after(now), Some(ist_at(7, 9, 15, 0)));
    assert_eq!(
        schedule(json!({"scheduleType": "daily", "time": "25:99"}), now),
        Err("Invalid time format: 25:99".into())
    );
    assert_eq!(
        schedule(json!({"scheduleType": "daily", "time": "09:15:30"}), now),
        Err("Invalid time format: 09:15:30".into())
    );
}

/// T-013: weekly on its days (0 = Monday), unknown entries dropped; none
/// left is refused.
#[test]
fn weekly_fires_on_its_days() {
    let now = ist_at(6, 10, 0, 0);
    let s = schedule(
        json!({"scheduleType": "weekly", "time": "15:00", "days": [2, 9, "4"]}),
        now,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        s.next_at_or_after(now),
        Some(ist_at(11, 15, 0, 0)),
        "Wednesday"
    );
    assert_eq!(
        schedule(json!({"scheduleType": "weekly", "days": [7, 8]}), now),
        Err("Invalid weekly schedule configuration".into())
    );
}

/// T-002, T-016: coalesced fires, the 60 s grace, one job per workflow,
/// a `once` job gone after its fire, removal of a missing job is not an
/// error.
#[test]
fn ticks_coalesce_and_skip_a_fire_more_than_a_minute_late() {
    let s = Scheduler::default();
    let at = ist_at(3, 9, 15, 0);
    let daily = Schedule::Daily {
        at: NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
        days: None,
    };
    assert_eq!(s.add(1, daily.clone(), true, ist_at(3, 9, 0, 0)), Some(at));
    assert_eq!(s.add(2, daily, false, ist_at(3, 9, 0, 0)), Some(at));
    s.add(3, Schedule::Once { at }, false, ist_at(3, 9, 0, 0));
    assert!(s.tick(ist_at(3, 9, 14, 59)).is_empty());
    let due = s.tick(at + Duration::seconds(30));
    assert_eq!(due.len(), 3);
    assert!(due.iter().all(|d| !d.missed && d.at == at));
    assert!(due[0].market_hours_only && !due[1].market_hours_only);
    assert_eq!(due[0].next, Some(ist_at(4, 9, 15, 0)));
    assert!(due[2].once && due[2].next.is_none());
    assert!(!s.has(3), "a one-time job is gone once fired");
    // Two days later, a minute and a half late: one fire, skipped.
    let late = s.tick(ist_at(5, 9, 16, 30));
    assert_eq!(late.len(), 2);
    assert!(late.iter().all(|d| d.missed && d.at == ist_at(5, 9, 15, 0)));
    assert!(s.remove(1));
    assert!(!s.remove(1));
    assert_eq!(s.workflow_ids(), vec![2]);
}

/// T-002 (decided, lead decision 7): a fire that fell due while the app
/// was closed runs once if the app starts within 60 s of it, else it is
/// skipped and the next one computed; a past one-time fire stays due so
/// the tick retires it.
#[test]
fn a_restart_catches_up_only_within_the_grace() {
    let daily = || Schedule::Daily {
        at: NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
        days: None,
    };
    let s = Scheduler::default();
    let recorded = Some(ist_at(2, 9, 15, 0));
    assert_eq!(
        s.restore(1, daily(), false, recorded, ist_at(3, 9, 15, 40)),
        Some(ist_at(3, 9, 15, 0)),
        "today's fire, 40 s ago"
    );
    let due = s.tick(ist_at(3, 9, 15, 40));
    assert!(!due[0].missed);
    assert_eq!(
        s.restore(2, daily(), false, recorded, ist_at(3, 9, 20, 0)),
        Some(ist_at(4, 9, 15, 0)),
        "five minutes late: skipped"
    );
    let once = Schedule::Once {
        at: ist_at(2, 10, 0, 0),
    };
    assert_eq!(
        s.restore(3, once, false, None, ist_at(3, 9, 0, 0)),
        Some(ist_at(2, 10, 0, 0))
    );
    let due = s.tick(ist_at(3, 9, 0, 0));
    assert!(due.iter().any(|d| d.workflow_id == 3 && d.missed && d.once));
}

/// The driver sleeps until the next fire, at most a minute.
#[test]
fn the_driver_sleeps_until_the_next_fire() {
    let s = Scheduler::default();
    let now = ist_at(3, 9, 0, 0);
    assert_eq!(s.sleep_for(now), std::time::Duration::from_secs(60));
    s.add(
        1,
        Schedule::Once {
            at: now + Duration::seconds(5),
        },
        false,
        now,
    );
    assert_eq!(s.sleep_for(now), std::time::Duration::from_secs(5));
}
