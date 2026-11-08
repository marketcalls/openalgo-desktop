use super::*;

fn failure(message: &str) -> SignInFailure {
    SignInFailure {
        broker: "zerodha".into(),
        message: message.into(),
    }
}

/// Read once, only by the session it came back with; an id is opaque.
#[test]
fn a_reason_is_read_once_by_its_own_session() {
    let f = SignInFailures::default();
    let now = Instant::now();
    let id = f.record(failure("Wrong keys."), Some("session-a"), now);
    assert!(
        id.len() >= 43,
        "a random id, as long as a session id: {}",
        id
    );
    assert!(!id.contains("Wrong"));
    assert_eq!(f.take(&id, "session-b", now), None, "not another session's");
    assert_eq!(f.take(&id, "session-a", now), Some(failure("Wrong keys.")));
    assert_eq!(f.take(&id, "session-a", now), None, "once only");
    assert_eq!(f.take("never-issued", "session-a", now), None);
}

/// A reason that came back without a session (an XTS post-back has no
/// cookie) is read once by whichever signed-in session asks.
#[test]
fn a_reason_without_a_session_is_read_once_by_any_session() {
    let f = SignInFailures::default();
    let now = Instant::now();
    let id = f.record(failure("Wrong keys."), None, now);
    assert_eq!(f.take(&id, "session-b", now), Some(failure("Wrong keys.")));
    assert_eq!(f.take(&id, "session-a", now), None);
}

#[test]
fn a_reason_expires() {
    let f = SignInFailures::default();
    let now = Instant::now();
    let id = f.record(failure("Late."), Some("s"), now);
    let later = now + FAILURE_TTL + Duration::from_secs(1);
    assert_eq!(f.take(&id, "s", later), None);
}

/// Bounded: a burst of failures keeps the newest `MAX_FAILURES`.
#[test]
fn reasons_are_bounded() {
    let f = SignInFailures::default();
    let t0 = Instant::now();
    let ids: Vec<String> = (0..MAX_FAILURES + 10)
        .map(|i| {
            let at = t0 + Duration::from_millis(i as u64);
            f.record(failure(&format!("reason {}", i)), Some("s"), at)
        })
        .collect();
    let now = t0 + Duration::from_secs(1);
    let kept = ids
        .iter()
        .filter(|id| f.take(id, "s", now).is_some())
        .count();
    assert_eq!(kept, MAX_FAILURES);
    let newest = f.record(failure("newest"), Some("s"), now);
    assert_eq!(f.take(&newest, "s", now), Some(failure("newest")));
}

/// Logout clears every reason, and a record racing the logout keeps
/// nothing (the cache's generation, read before the fill).
#[test]
fn logout_clears_reasons_and_a_racing_record_keeps_nothing() {
    let f = SignInFailures::default();
    let now = Instant::now();
    let id = f.record(failure("Before logout."), Some("s"), now);
    f.clear();
    assert_eq!(f.take(&id, "s", now), None);

    let generation = f.kept.generation();
    f.clear(); // the logout lands between the read and the fill
    let racing = "racing".to_string();
    let kept = Kept {
        failure: failure("During logout."),
        owner: Some("s".into()),
    };
    assert!(!f.kept.fill(racing.clone(), kept, generation, now));
    assert_eq!(f.take(&racing, "s", now), None);
}
