//! Sahi's answers to a connection ending, from the web's `_run` loop
//! (broker/sahi/streaming/sahi_websocket.py), which has no test of its own.

use super::*;

fn closed(code: u16) -> Ended<'static> {
    Ended::Closed {
        code: Some(code),
        reason: "",
    }
}

fn after(s: u64) -> Reconnect {
    Reconnect::After(Duration::from_secs(s))
}

fn stop_kind(r: &Reconnect) -> Option<StopKind> {
    match r {
        Reconnect::Stop { kind, .. } => Some(*kind),
        _ => None,
    }
}

/// Web sahi_websocket.py `_run`, 4429: 60, 120, 240, 300 and 300 s, then
/// it stops with the trader told to close the other streaming apps.
#[tokio::test(start_paused = true)]
async fn too_many_connections_waits_longer_each_time_then_stops() {
    let mut p = ClosePolicy::default();
    let mut waits = Vec::new();
    for _ in 0..5 {
        p.opened();
        waits.push(p.ended(closed(4429)));
    }
    assert_eq!(waits, [60, 120, 240, 300, 300].map(after));
    p.opened();
    let stop = p.ended(closed(4429));
    assert_eq!(stop_kind(&stop), Some(StopKind::ConnectionLimit));
    let Reconnect::Stop { message, .. } = stop else {
        unreachable!()
    };
    assert!(
        message.contains("Close other Sahi streaming apps or sessions, then reconnect"),
        "{}",
        message
    );
}

/// The trader reconnecting after the stop, or a connection that stayed up
/// 30 s (web `STABLE_AFTER`), starts the 4429 count again.
#[tokio::test(start_paused = true)]
async fn a_resume_or_a_stable_connection_starts_the_count_again() {
    let mut p = ClosePolicy::default();
    for _ in 0..6 {
        p.opened();
        p.ended(closed(4429));
    }
    p.resume();
    p.opened();
    assert_eq!(p.ended(closed(4429)), after(60));
    p.opened();
    assert_eq!(p.ended(closed(4429)), after(120));
    p.opened();
    tokio::time::advance(Duration::from_secs(31)).await;
    assert_eq!(p.ended(closed(4408)), Reconnect::Backoff);
    p.opened();
    assert_eq!(p.ended(closed(4429)), after(60));
}

/// Web `_AUTH_CODES`: 4401 and 4403 stop until the trader logs in again
/// (the desktop's token cannot change under a running feed, so the web's
/// re-read of the stored token finds the same one and stops at once).
#[test]
fn a_dead_token_stops_for_a_new_login() {
    for code in [4401, 4403] {
        let mut p = ClosePolicy::default();
        assert_eq!(
            stop_kind(&p.ended(closed(code))),
            Some(StopKind::Auth),
            "{}",
            code
        );
    }
}

/// Web `_run`: an upgrade refused for the rate limit waits the 30 s cap;
/// a normal close, a ping timeout, a slow consumer or a read error takes
/// the usual backoff.
#[test]
fn a_rate_limited_upgrade_waits_30_s_and_other_closes_back_off() {
    let mut p = ClosePolicy::default();
    assert_eq!(p.ended(Ended::Refused(429)), after(30));
    for code in [1000, 4408, 4409] {
        assert_eq!(p.ended(closed(code)), Reconnect::Backoff, "{}", code);
    }
    let read_error = Ended::Closed {
        code: None,
        reason: "",
    };
    assert_eq!(p.ended(read_error), Reconnect::Backoff);
    assert_eq!(p.ended(Ended::Refused(503)), Reconnect::Backoff);
}
