//! Owners and the source's answers, at the registry (web
//! test/test_websocket_unsubscribe_contract.py at a1b9e3250). The wire
//! acknowledgements of the same cases are in `tests/it/feed_lifecycle.rs`.

use super::super::{Registry, RegistryStats};
use super::Release;
use crate::feed::outbox::Outbox;
use crate::feed::source::{FakeSource, InstrumentKey, Mode, SourceCall, SourceOutcome};
use futures_util::FutureExt;
use std::sync::Arc;

fn reliance() -> InstrumentKey {
    InstrumentKey::new("RELIANCE", "NSE")
}

fn registry(src: &Arc<FakeSource>, clients: &[u64]) -> Registry {
    let r = Registry::new(src.clone(), 10);
    for id in clients {
        r.add_client(*id, Arc::new(Outbox::new(8, 8)));
    }
    r
}

/// A subscribe as the session runs it: hold, wait for the source, settle.
fn subscribe(r: &Registry, id: u64, k: &InstrumentKey, mode: Mode, depth: u8) -> SourceOutcome {
    let admission = r.subscribe(id, k, mode, depth).expect("held");
    let replacing = admission.replacing();
    let outcome = admission.answer().now_or_never().expect("answered at once");
    r.settle_subscribe(id, k, mode, depth, replacing, &outcome);
    outcome
}

/// An unsubscribe as the session runs it; `None` while the source has not
/// answered (the release stays with the source, as after a reply
/// deadline: its answer, if it comes, settles it).
fn unsubscribe(r: &Registry, id: u64, k: &InstrumentKey, mode: Mode) -> Option<SourceOutcome> {
    match r.begin_unsubscribe(id, k, mode) {
        Release::Done => Some(Ok(())),
        Release::Waiting { answer, ticket } => {
            let outcome = answer.now_or_never();
            if let Some(o) = &outcome {
                r.finish_unsubscribe(id, k, mode, ticket, o);
            }
            outcome
        }
    }
}

fn holds(r: &Registry) -> usize {
    r.stats().client_subscriptions
}

/// Web test_last_subscriber_failure_preserves_both_registries_and_retry_success_removes_them:
/// the broker refuses the last owner's release, so the owner and the
/// source key stay; the retry is released once.
#[test]
fn a_refused_release_keeps_the_owner_and_a_retry_releases_it_once() {
    let src = FakeSource::new([reliance()]);
    let r = registry(&src, &[7]);
    subscribe(&r, 7, &reliance(), Mode::Ltp, 5).unwrap();
    src.fail_release(reliance(), Mode::Ltp, "broker refused");

    assert_eq!(
        unsubscribe(&r, 7, &reliance(), Mode::Ltp),
        Some(Err("broker refused".into()))
    );
    assert_eq!((holds(&r), r.stats().source_keys), (1, 1));
    assert_eq!(src.active_count(), 1);

    assert_eq!(unsubscribe(&r, 7, &reliance(), Mode::Ltp), Some(Ok(())));
    assert_eq!((holds(&r), r.stats().source_keys), (0, 0));
    assert_eq!(src.active_count(), 0);
    let releases = src
        .calls()
        .into_iter()
        .filter(|c| matches!(c, SourceCall::Unsubscribe(..)))
        .count();
    assert_eq!(releases, 2, "the refused release and its retry");
    assert!(src.violations().is_empty(), "{:?}", src.violations());
}

/// Web test_invalid_or_exceptional_broker_response_preserves_exact_ownership:
/// a release the source never answers keeps the owner (the session gives
/// up waiting), and the next unsubscribe asks again.
#[test]
fn an_unanswered_release_keeps_the_owner() {
    let src = FakeSource::new([reliance()]);
    let r = registry(&src, &[7]);
    subscribe(&r, 7, &reliance(), Mode::Ltp, 5).unwrap();
    src.hold_release(reliance(), Mode::Ltp);

    assert_eq!(unsubscribe(&r, 7, &reliance(), Mode::Ltp), None);
    assert_eq!((holds(&r), r.stats().source_keys), (1, 1));
    assert_eq!(unsubscribe(&r, 7, &reliance(), Mode::Ltp), Some(Ok(())));
    assert_eq!(
        r.stats(),
        RegistryStats {
            clients: 1,
            ..Default::default()
        }
    );
}

/// Web test_multi_client_unsubscribe_removes_only_the_caller_until_the_last_ack:
/// an owner others share the key with goes locally, with no broker call;
/// the last one is released at the broker.
#[test]
fn a_shared_owner_goes_locally_until_the_last() {
    let src = FakeSource::new([reliance()]);
    let r = registry(&src, &[7, 8]);
    for id in [7, 8] {
        subscribe(&r, id, &reliance(), Mode::Depth, 5).unwrap();
    }
    src.fail_release(reliance(), Mode::Depth, "would fail if asked");

    assert_eq!(unsubscribe(&r, 7, &reliance(), Mode::Depth), Some(Ok(())));
    assert_eq!(holds(&r), 1);
    assert!(
        !src.calls()
            .iter()
            .any(|c| matches!(c, SourceCall::Unsubscribe(..))),
        "no broker call while another owner holds it"
    );
    assert_eq!(
        unsubscribe(&r, 8, &reliance(), Mode::Depth),
        Some(Err("would fail if asked".into()))
    );
    assert_eq!(unsubscribe(&r, 8, &reliance(), Mode::Depth), Some(Ok(())));
    assert_eq!(src.active_count(), 0);
}

/// Mixed modes on one instrument are released one mode at a time: a
/// refused LTP release keeps LTP and does not touch Quote.
#[test]
fn mixed_modes_release_exactly_their_own_mode() {
    let src = FakeSource::new([reliance()]);
    let r = registry(&src, &[7]);
    subscribe(&r, 7, &reliance(), Mode::Ltp, 5).unwrap();
    subscribe(&r, 7, &reliance(), Mode::Quote, 5).unwrap();
    src.fail_release(reliance(), Mode::Ltp, "ltp still live");

    assert_eq!(
        unsubscribe(&r, 7, &reliance(), Mode::Ltp),
        Some(Err("ltp still live".into()))
    );
    assert_eq!(unsubscribe(&r, 7, &reliance(), Mode::Quote), Some(Ok(())));
    assert_eq!(src.active(), vec![(reliance(), Mode::Ltp, 5)]);
    assert_eq!(holds(&r), 1);
}

/// Web test_unsubscribe_all_failure_preserves_ownership_until_retry_succeeds:
/// `unsubscribe_all` keeps what the broker would not release and lets go
/// of the rest; the retry releases what was kept.
#[test]
fn unsubscribe_all_keeps_only_what_the_broker_refused() {
    let sbin = InstrumentKey::new("SBIN", "NSE");
    let src = FakeSource::new([reliance(), sbin.clone()]);
    let r = registry(&src, &[7]);
    subscribe(&r, 7, &reliance(), Mode::Ltp, 5).unwrap();
    subscribe(&r, 7, &sbin, Mode::Quote, 5).unwrap();
    src.fail_release(reliance(), Mode::Ltp, "still live");

    let outcomes: Vec<_> = r
        .begin_unsubscribe_all(7)
        .into_iter()
        .map(|(k, mode, release)| {
            let outcome = match release {
                Release::Done => Ok(()),
                Release::Waiting { answer, ticket } => {
                    let o = answer.now_or_never().expect("answered");
                    r.finish_unsubscribe(7, &k, mode, ticket, &o);
                    o
                }
            };
            (k.symbol, outcome)
        })
        .collect();
    assert_eq!(
        outcomes,
        vec![
            ("RELIANCE".to_string(), Err("still live".to_string())),
            ("SBIN".to_string(), Ok(())),
        ]
    );
    assert_eq!(src.active(), vec![(reliance(), Mode::Ltp, 5)]);

    let again: Vec<_> = r
        .begin_unsubscribe_all(7)
        .into_iter()
        .map(|(k, mode, release)| {
            let Release::Waiting { answer, ticket } = release else {
                panic!("RELIANCE is still the client's alone")
            };
            let outcome = answer.now_or_never().expect("answered");
            r.finish_unsubscribe(7, &k, mode, ticket, &outcome);
            outcome
        })
        .collect();
    assert_eq!(again, vec![Ok(())]);
    assert_eq!((src.active_count(), holds(&r)), (0, 0));
}

/// Web test_single_disconnect_cleanup_falls_back_to_adapter_teardown_after_exact_release_failure
/// and test_failed_release_with_another_live_client_is_reclaimed_at_real_last_disconnect:
/// a disconnect is terminal, so its owner goes whatever the broker
/// answers; the source keeps releasing on its own, and a key another
/// client holds is not released.
#[test]
fn a_disconnect_drops_its_owners_whatever_the_broker_answers() {
    let sbin = InstrumentKey::new("SBIN", "NSE");
    let src = FakeSource::new([reliance(), sbin.clone()]);
    let r = registry(&src, &[7, 8]);
    subscribe(&r, 7, &reliance(), Mode::Ltp, 5).unwrap();
    subscribe(&r, 7, &sbin, Mode::Ltp, 5).unwrap();
    subscribe(&r, 8, &sbin, Mode::Ltp, 5).unwrap();
    src.fail_release(reliance(), Mode::Ltp, "exact refusal");

    r.remove_client(7);
    assert_eq!(r.stats().clients, 1);
    assert_eq!(holds(&r), 1, "client 8 still holds SBIN");
    assert_eq!(r.stats().source_keys, 1);
    assert_eq!(
        src.active(),
        vec![(reliance(), Mode::Ltp, 5), (sbin.clone(), Mode::Ltp, 5)],
        "the refused release stays at the source, SBIN stays for client 8"
    );
    r.remove_client(8);
    assert_eq!(r.stats(), RegistryStats::default());
    assert_eq!(src.active(), vec![(reliance(), Mode::Ltp, 5)]);
}

/// An owner that joins while the last owner's release is with the source
/// asks the source to hold the key again: the release answers for the
/// leaving owner alone, and the key stays for the one that joined.
#[test]
fn an_owner_joining_during_a_release_keeps_the_key() {
    let src = FakeSource::new([reliance()]);
    let r = registry(&src, &[7, 8]);
    subscribe(&r, 7, &reliance(), Mode::Quote, 5).unwrap();
    let Release::Waiting(release) = r.begin_unsubscribe(7, &reliance(), Mode::Quote) else {
        panic!("the last owner waits for the source")
    };
    assert_eq!(subscribe(&r, 8, &reliance(), Mode::Quote, 5), Ok(()));
    let outcome = release.now_or_never().expect("answered");
    r.finish_unsubscribe(7, &reliance(), Mode::Quote, &outcome);
    assert_eq!(outcome, Ok(()));
    assert_eq!(holds(&r), 1);
    assert_eq!(src.active(), vec![(reliance(), Mode::Quote, 5)]);
    assert!(src.violations().is_empty(), "{:?}", src.violations());

    r.remove_client(8);
    assert_eq!(src.active_count(), 0, "the last owner's leave releases it");
    assert!(src.violations().is_empty(), "{:?}", src.violations());
}

/// Audit C02: a subscribe the source refuses is answered with why, holds
/// nothing, and asks the source for no release; an owner that comes later
/// asks again.
#[test]
fn a_refused_subscribe_holds_nothing() {
    let src = FakeSource::new([reliance()]);
    let r = registry(&src, &[7, 8]);
    src.set_capacity(0);
    assert_eq!(
        subscribe(&r, 7, &reliance(), Mode::Ltp, 5),
        Err("You can stream at most 0 instruments at once.".into())
    );
    assert_eq!(
        r.stats(),
        RegistryStats {
            clients: 2,
            ..Default::default()
        }
    );
    assert!(!src
        .calls()
        .iter()
        .any(|c| matches!(c, SourceCall::Unsubscribe(..))));

    src.set_capacity(1);
    assert_eq!(subscribe(&r, 8, &reliance(), Mode::Ltp, 5), Ok(()));
    assert_eq!(src.active_count(), 1);
    assert!(src.violations().is_empty(), "{:?}", src.violations());
}

/// A subscription moved to a depth the source refuses keeps the depth it
/// had, and keeps streaming it.
#[test]
fn a_refused_depth_move_keeps_the_old_depth() {
    let src = FakeSource::new([reliance()]);
    let r = registry(&src, &[7]);
    subscribe(&r, 7, &reliance(), Mode::Depth, 5).unwrap();
    src.set_capacity(1);
    src.refuse(reliance(), "20-level depth is not available right now");
    assert!(subscribe(&r, 7, &reliance(), Mode::Depth, 20).is_err());
    assert_eq!(src.active(), vec![(reliance(), Mode::Depth, 5)]);
    assert_eq!(unsubscribe(&r, 7, &reliance(), Mode::Depth), Some(Ok(())));
    assert_eq!(src.active_count(), 0);
}
