//! The queue's rules, call by call (web
//! test/test_gthread_sandbox_lifecycle.py, 398f614cc). The worker and the
//! engine around it are tested in `tests/it/sandbox_feed_queue.rs`.

use super::*;
use crate::brokers::common::streaming::FeedMode;

fn sub(symbol: &str) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        token: "1".into(),
        brsymbol: symbol.into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Ltp,
        depth: 5,
    }
}

fn watch(symbol: &str) -> WatchCmd {
    WatchCmd::Watch(sub(symbol))
}

fn unwatch(symbol: &str) -> WatchCmd {
    WatchCmd::Unwatch(sub(symbol))
}

fn resubscribe(symbol: &str) -> WatchCmd {
    WatchCmd::Resubscribe(sub(symbol))
}

fn waiting(calls: &FeedCalls) -> Vec<WatchCmd> {
    calls.waiting.lock().calls.iter().cloned().collect()
}

/// Web ::test_a_subscribe_still_waiting_is_cancelled_by_its_unsubscribe.
#[test]
fn a_subscribe_still_waiting_is_cancelled_by_its_release() {
    let calls = FeedCalls::default();
    calls.push(watch("RELIANCE")).unwrap();
    assert_eq!(waiting(&calls).len(), 1);
    calls.push(unwatch("RELIANCE")).unwrap();
    assert!(waiting(&calls).is_empty());
}

/// Web ::test_an_unsubscribe_then_a_subscribe_are_both_kept.
#[test]
fn a_release_then_a_subscribe_are_both_kept() {
    let calls = FeedCalls::default();
    calls.push(unwatch("RELIANCE")).unwrap();
    calls.push(watch("RELIANCE")).unwrap();
    assert_eq!(waiting(&calls), [unwatch("RELIANCE"), watch("RELIANCE")]);
}

/// Web ::test_the_queue_stays_bounded_while_the_feed_does_not_answer.
#[test]
fn the_queue_stays_bounded_while_the_feed_does_not_answer() {
    let calls = FeedCalls::default();
    calls.push(unwatch("RELIANCE")).unwrap();
    let mut longest = 0;
    for _ in 0..500 {
        calls.push(watch("RELIANCE")).unwrap();
        longest = longest.max(waiting(&calls).len());
        calls.push(unwatch("RELIANCE")).unwrap();
        longest = longest.max(waiting(&calls).len());
    }
    assert!(longest <= 2, "{longest} calls waited for one symbol");
    assert_eq!(waiting(&calls), [unwatch("RELIANCE")]);
}

/// Web ::test_calls_for_several_symbols_are_never_cancelled, as it applies
/// here: every call is for one symbol, and nothing waiting for another
/// symbol is cancelled or joined.
#[test]
fn a_call_for_another_symbol_is_never_cancelled() {
    let calls = FeedCalls::default();
    calls.push(watch("RELIANCE")).unwrap();
    calls.push(watch("ZEEL")).unwrap();
    calls.push(unwatch("RELIANCE")).unwrap();
    calls.push(unwatch("SBIN")).unwrap();
    assert_eq!(waiting(&calls), [watch("ZEEL"), unwatch("SBIN")]);
}

#[test]
fn a_call_joins_one_of_its_kind_already_waiting() {
    let calls = FeedCalls::default();
    calls.push(watch("RELIANCE")).unwrap();
    calls.push(resubscribe("RELIANCE")).unwrap();
    calls.push(watch("RELIANCE")).unwrap();
    calls.push(unwatch("ZEEL")).unwrap();
    calls.push(unwatch("ZEEL")).unwrap();
    assert_eq!(waiting(&calls), [watch("RELIANCE"), unwatch("ZEEL")]);
}

/// A resubscribe lets go of what is held before it subscribes, so the
/// release that cancels it still has to let go.
#[test]
fn a_release_takes_the_place_of_a_resubscribe_still_waiting() {
    let calls = FeedCalls::default();
    calls.push(resubscribe("RELIANCE")).unwrap();
    calls.push(watch("ZEEL")).unwrap();
    calls.push(unwatch("RELIANCE")).unwrap();
    assert_eq!(waiting(&calls), [unwatch("RELIANCE"), watch("ZEEL")]);

    // With a release already waiting before it, that one does.
    let calls = FeedCalls::default();
    calls.push(unwatch("RELIANCE")).unwrap();
    calls.push(resubscribe("RELIANCE")).unwrap();
    calls.push(unwatch("RELIANCE")).unwrap();
    assert_eq!(waiting(&calls), [unwatch("RELIANCE")]);
}

/// Subscribes are capped and counted when turned away; releases may use
/// the room above the cap, and are capped too.
#[test]
fn a_full_queue_turns_subscribes_away_and_counts_them() {
    let calls = FeedCalls::default();
    for n in 0..CAPACITY {
        calls.push(watch(&format!("S{n}"))).unwrap();
    }
    assert_eq!(calls.push(watch("LATE")), Err(Refused::Full));
    assert_eq!(calls.push(resubscribe("LATER")), Err(Refused::Full));
    assert_eq!(calls.turned_away(), 2);

    for n in 0..CAPACITY {
        calls.push(unwatch(&format!("HELD{n}"))).unwrap();
    }
    assert_eq!(waiting(&calls).len(), RELEASE_CAPACITY);
    assert_eq!(calls.push(unwatch("LAST")), Err(Refused::Full));
    assert_eq!(calls.turned_away(), 3);

    // A release that cancels a waiting subscribe needs no room.
    calls.push(unwatch("S0")).unwrap();
    assert_eq!(waiting(&calls).len(), RELEASE_CAPACITY - 1);
}

#[tokio::test]
async fn the_worker_takes_calls_oldest_first_and_nothing_once_closed() {
    let calls = std::sync::Arc::new(FeedCalls::default());
    let taker = {
        let calls = calls.clone();
        tokio::spawn(async move { calls.next().await })
    };
    calls.push(watch("RELIANCE")).unwrap();
    calls.push(watch("ZEEL")).unwrap();
    assert_eq!(taker.await.unwrap(), Some(watch("RELIANCE")));

    calls.close();
    assert_eq!(calls.next().await, None);
    assert_eq!(calls.push(watch("SBIN")), Err(Refused::Closed));
    assert!(waiting(&calls).is_empty());
}
