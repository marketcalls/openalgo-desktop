//! Web `test/test_flattrade_data_silence_watchdog.py`, the same inputs on
//! a fake clock (milliseconds; the web's clock starts at 1_700_000_000 s).

use super::*;

const START: i64 = 1_700_000_000_000;
const HEARTBEAT_MS: i64 = 30_000;

/// `feed_live_ticks`: a tick every 5 s for 120 s; the clock after it.
fn live_ticks(l: &mut Liveness, mut now: i64) -> i64 {
    for _ in 0..24 {
        l.note_data(now);
        now += 5_000;
    }
    now
}

/// Web `test_a_session_that_only_answers_heartbeats_is_recycled`: after
/// live ticks only heartbeats come; the session is called stalled at the
/// sixth 30 s check (185 s after the last tick), never before.
#[test]
fn a_session_that_only_answers_heartbeats_is_called_stalled() {
    let mut l = Liveness::default();
    let mut now = live_ticks(&mut l, START);
    let mut verdicts = Vec::new();
    for _ in 0..6 {
        now += HEARTBEAT_MS;
        verdicts.push(l.stalled_at(now));
    }
    assert_eq!(verdicts, [false, false, false, false, false, true]);
}

/// Web `test_a_feed_that_keeps_delivering_is_left_alone`.
#[test]
fn a_feed_that_keeps_delivering_is_never_stalled() {
    let mut l = Liveness::default();
    let mut now = live_ticks(&mut l, START);
    for _ in 0..10 {
        now += HEARTBEAT_MS;
        l.note_data(now);
        assert!(!l.stalled_at(now));
    }
}

/// Web `test_a_subscribe_time_snapshot_burst_does_not_arm_the_watchdog`:
/// fifty snapshots in one second, then hours of quiet.
#[test]
fn a_snapshot_burst_does_not_arm_it() {
    let mut l = Liveness::default();
    let mut now = START;
    for _ in 0..50 {
        l.note_data(now);
        now += 20;
    }
    for _ in 0..200 {
        now += HEARTBEAT_MS;
        assert!(!l.stalled_at(now));
    }
}

/// Web `test_arming_needs_spread_not_volume`: three buckets arm it, five
/// hundred frames in one instant do not.
#[test]
fn arming_needs_spread_not_volume() {
    let mut l = Liveness::default();
    let mut now = START;
    for _ in 0..500 {
        l.note_data(now);
    }
    assert!(!l.stalled_at(now + SILENCE_MS + 1));
    now += ARM_BUCKET_MS + 1_000;
    l.note_data(now);
    assert!(
        !l.stalled_at(now + SILENCE_MS + 1),
        "two buckets is not yet flow"
    );
    now += ARM_BUCKET_MS + 1_000;
    l.note_data(now);
    assert!(!l.stalled_at(now + SILENCE_MS));
    assert!(l.stalled_at(now + SILENCE_MS + 1));
}

/// Web `test_stray_ticks_hours_apart_do_not_arm`.
#[test]
fn stray_ticks_hours_apart_do_not_arm_it() {
    let mut l = Liveness::default();
    let mut now = START;
    for _ in 0..5 {
        l.note_data(now);
        now += 3_600_000;
    }
    assert!(!l.stalled_at(now + SILENCE_MS + 1));
}

/// Web `test_a_reconnect_starts_the_watchdog_disarmed`: a new connection
/// sits quietly until data flows again.
#[test]
fn a_new_connection_starts_disarmed() {
    let mut l = Liveness::default();
    let now = live_ticks(&mut l, START);
    assert!(l.stalled_at(now + SILENCE_MS));
    l.reset();
    assert!(!l.stalled_at(now + 10 * SILENCE_MS));
}
