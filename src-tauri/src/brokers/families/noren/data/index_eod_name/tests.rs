//! The index-name cache follows `crate::cache::TtlCache`'s rules: bounded,
//! a TTL, and a clear that a lookup in flight cannot undo. The lookups
//! themselves run through the history in `brokers::tradesmart`.

use super::*;

fn key(token: &str) -> (String, String) {
    ("NSE".to_string(), token.to_string())
}

/// 64 names, each kept a day (the web's `LockedTTLCache(maxsize=64,
/// ttl=24 * 3600)`).
#[test]
fn the_names_are_bounded_and_kept_a_day() {
    let names = IndexEodNames::default();
    assert_eq!(names.0.maxsize(), 64);
    assert_eq!(names.0.ttl(), Duration::from_secs(24 * 3600));
    let now = Instant::now();
    for token in 0..100 {
        names.0.fill(
            key(&token.to_string()),
            "Nifty 50".into(),
            names.0.generation(),
            now,
        );
    }
    assert_eq!(names.0.len(), 64);
    let later = now + Duration::from_secs(24 * 3600);
    assert_eq!(names.0.get(&key("99"), later), None, "a day old");
}

/// A name read before a logout cleared the cache is not kept.
#[test]
fn a_name_read_across_a_clear_is_not_kept() {
    let names = IndexEodNames::default();
    let now = Instant::now();
    let (cached, generation) = names.0.get_with_generation(&key("26000"), now);
    assert_eq!(cached, None);
    names.clear();
    assert!(!names
        .0
        .fill(key("26000"), "Nifty 50".into(), generation, now));
    assert!(names.0.is_empty());
}
