//! The broker's step before a new instrument is subscribed (bk-c RB-05,
//! Samco's cold index): awaited for new instruments only, its prepared
//! fields streamed, its refusals and a step past the deadline answered per
//! item, with no lock held while it runs.

use super::presubscribe::{PRESUBSCRIBE_DEADLINE, PRESUBSCRIBE_LATE};
use super::tests::fast;
use super::*;
use crate::brokers::common::streaming::{FeedLetGo, FeedMode, Presubscribe};
use futures_util::FutureExt;

fn sub(symbol: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: "NSE_INDEX".into(),
        token: String::new(),
        brsymbol: symbol.into(),
        brexchange: "NSE".into(),
        mode,
        depth: 5,
    }
}

/// A step that gives NIFTY its streaming id, refuses BAD, leaves the rest,
/// and records each call (the instruments it was asked for).
fn step(calls: Arc<Mutex<Vec<Vec<String>>>>) -> Presubscribe {
    Arc::new(move |subs: Vec<FeedSubscription>| {
        calls
            .lock()
            .push(subs.iter().map(|s| s.symbol.clone()).collect());
        let out = subs
            .into_iter()
            .map(|mut s| match s.symbol.as_str() {
                "BAD" => Err("Symbol BAD not found".to_string()),
                "NIFTY" => {
                    s.token = "26000".into();
                    // Only the broker's fields count: a changed mode is not
                    // taken.
                    s.mode = FeedMode::Depth;
                    Ok(s)
                }
                _ => Ok(s),
            })
            .collect();
        async move { out }.boxed()
    })
}

#[tokio::test]
async fn a_new_instrument_streams_as_the_broker_prepared_it_and_its_refusal_is_its_answer() {
    let m = WebSocketManager::with_config(fast());
    let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    m.set_presubscribe(Some(step(calls.clone())));
    let answers = m
        .admit(vec![
            sub("NIFTY", FeedMode::Ltp),
            sub("BAD", FeedMode::Ltp),
            sub("BANKNIFTY", FeedMode::Ltp),
        ])
        .await;
    assert_eq!(
        answers,
        vec![Ok(()), Err("Symbol BAD not found".to_string()), Ok(())]
    );
    let nifty = m
        .subscriptions()
        .into_iter()
        .find(|s| s.symbol == "NIFTY")
        .expect("NIFTY held");
    assert_eq!((nifty.token.as_str(), nifty.mode), ("26000", FeedMode::Ltp));
    assert_eq!(m.instrument_count(), 2);

    // Held already: no second lookup, for another mode either.
    assert_eq!(
        m.admit(vec![sub("NIFTY", FeedMode::Quote)]).await,
        vec![Ok(())]
    );
    assert_eq!(calls.lock().len(), 1);
    assert_eq!(calls.lock()[0], ["NIFTY", "BAD", "BANKNIFTY"]);

    // The all-or-none subscribe refuses the batch with the step's words.
    let refused = m
        .subscribe(vec![
            sub("SENSEX", FeedMode::Ltp),
            sub("BAD", FeedMode::Ltp),
        ])
        .await;
    assert!(
        matches!(&refused, Err(AppError::Validation(why)) if why == "Symbol BAD not found"),
        "{:?}",
        refused
    );
    assert_eq!(m.instrument_count(), 2);

    // A disconnect forgets the step with the session.
    m.disconnect().await.unwrap();
    assert_eq!(m.admit(vec![sub("BAD", FeedMode::Ltp)]).await, vec![Ok(())]);
}

#[tokio::test(start_paused = true)]
async fn a_step_past_the_deadline_refuses_its_items() {
    let m = WebSocketManager::with_config(fast());
    let slow: Presubscribe = Arc::new(|subs: Vec<FeedSubscription>| {
        async move {
            tokio::time::sleep(PRESUBSCRIBE_DEADLINE * 6).await;
            subs.into_iter().map(Ok).collect()
        }
        .boxed()
    });
    m.set_presubscribe(Some(slow));
    let answers = m.admit(vec![sub("NIFTY", FeedMode::Ltp)]).await;
    assert_eq!(answers, vec![Err(PRESUBSCRIBE_LATE.to_string())]);
    assert_eq!(m.instrument_count(), 0);
}

/// IndMoney (bk-b RB-13): what the broker's web adapter refuses is refused
/// for every consumer of the manager, Flow and the sandbox as well as the
/// feed clients, in its words.
#[tokio::test]
async fn what_the_broker_refuses_is_refused_for_every_consumer() {
    let m = WebSocketManager::with_config(fast());
    let words = "Invalid mode 3. INDmoney supports only 1 (LTP) or 2 (Quote)";
    m.set_refusal(Some(Arc::new(move |s: &FeedSubscription| {
        (s.mode == FeedMode::Depth).then(|| words.to_string())
    })));
    let answers = m
        .admit(vec![
            sub("NIFTY", FeedMode::Ltp),
            sub("NIFTY", FeedMode::Depth),
        ])
        .await;
    assert_eq!(answers, vec![Ok(()), Err(words.to_string())]);
    let refused = m.subscribe(vec![sub("SBIN", FeedMode::Depth)]).await;
    assert!(
        matches!(&refused, Err(AppError::Validation(why)) if why == words),
        "{:?}",
        refused
    );
    assert_eq!(m.instrument_count(), 1);
    m.disconnect().await.unwrap();
    assert_eq!(
        m.admit(vec![sub("SBIN", FeedMode::Depth)]).await,
        vec![Ok(())]
    );
}

/// Dhan's 20-level socket: past its room the broker's own words
/// (dhan_adapter.py:395), else the web pool's.
#[tokio::test]
async fn a_full_feed_refuses_in_the_brokers_words_where_it_has_its_own() {
    let m = WebSocketManager::with_config(FeedConfig {
        max_instruments: 1,
        ..fast()
    });
    assert_eq!(
        m.admit(vec![sub("NIFTY", FeedMode::Depth)]).await,
        vec![Ok(())]
    );
    let pool = m.admit(vec![sub("BANKNIFTY", FeedMode::Depth)]).await;
    assert_eq!(
        pool,
        vec![Err(
            "Maximum capacity reached: 1 connections \u{d7} 1 symbols = 1 symbols. \
                  Currently subscribed to 1 symbols."
                .to_string()
        )]
    );
    m.set_room_refusal(Some("Maximum 50 subscriptions allowed for 20-depth"));
    assert_eq!(
        m.admit(vec![sub("BANKNIFTY", FeedMode::Depth)]).await,
        vec![Err(
            "Maximum 50 subscriptions allowed for 20-depth".to_string()
        )]
    );
}

/// Records each batch the broker's session is told no consumer holds.
fn recorder(told: Arc<Mutex<Vec<Vec<String>>>>) -> FeedLetGo {
    Arc::new(move |subs: &[FeedSubscription]| {
        told.lock()
            .push(subs.iter().map(|s| s.symbol.clone()).collect());
    })
}

/// With no socket up nothing carries a release: the session hears it at
/// once, for the instruments no consumer holds any more, from a waited
/// release and from a plain one.
#[tokio::test]
async fn a_release_no_socket_carries_tells_the_session_what_nobody_holds() {
    let m = WebSocketManager::with_config(fast());
    let told: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    m.set_let_go(Some(recorder(told.clone())));
    m.admit(vec![
        sub("A", FeedMode::Ltp),
        sub("A", FeedMode::Quote),
        sub("B", FeedMode::Ltp),
    ])
    .await;

    let answers = m
        .release(vec![sub("A", FeedMode::Ltp), sub("B", FeedMode::Ltp)])
        .await;
    assert!(answers.iter().all(|a| a.is_ok()));
    // A is still held in Quote.
    assert_eq!(*told.lock(), vec![vec!["B".to_string()]]);

    m.unsubscribe(vec![sub("A", FeedMode::Quote)])
        .await
        .unwrap();
    assert_eq!(
        *told.lock(),
        vec![vec!["B".to_string()], vec!["A".to_string()]]
    );
}

#[tokio::test]
async fn unsubscribing_everything_tells_the_session_of_every_instrument() {
    let m = WebSocketManager::with_config(fast());
    let told: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    m.set_let_go(Some(recorder(told.clone())));
    m.admit(vec![sub("A", FeedMode::Ltp)]).await;
    m.unsubscribe_all().await.unwrap();
    assert_eq!(*told.lock(), vec![vec!["A".to_string()]]);
}

/// What a waited release did not let go of (the socket did not take it,
/// or the broker refused) stays held, so its key stays too.
#[tokio::test]
async fn a_refused_release_is_not_told_as_let_go() {
    let m = WebSocketManager::with_config(fast());
    let told: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    m.set_let_go(Some(recorder(told.clone())));
    m.tell_let_go_answered(
        &[sub("A", FeedMode::Ltp), sub("B", FeedMode::Ltp)],
        &[Err("still subscribed".to_string()), Ok(())],
    );
    assert_eq!(*told.lock(), vec![vec!["B".to_string()]]);
}

/// An instrument placed again before the session is told keeps what it
/// holds, and a disconnect ends the telling.
#[tokio::test]
async fn an_instrument_held_again_is_not_told_and_a_disconnect_clears_the_hook() {
    let m = WebSocketManager::with_config(fast());
    let told: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    m.set_let_go(Some(recorder(told.clone())));
    m.admit(vec![sub("A", FeedMode::Ltp)]).await;
    m.tell_let_go(&[sub("A", FeedMode::Ltp)]);
    assert!(told.lock().is_empty());

    m.disconnect().await.unwrap();
    assert!(m.let_go.lock().is_none());
}

/// The session hears what the manager placed, from an admission and from
/// an all-or-none subscribe, and nothing of what it refused for room.
#[tokio::test]
async fn a_placed_instrument_is_told_held_and_one_refused_for_room_is_not() {
    let m = WebSocketManager::with_config(fast());
    m.registry.lock().set_limits(PoolLimits::single(1));
    let told: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    m.set_held(Some(recorder(told.clone())));

    let answers = m
        .admit(vec![sub("A", FeedMode::Ltp), sub("B", FeedMode::Ltp)])
        .await;
    assert!(answers[0].is_ok() && answers[1].is_err());
    assert_eq!(*told.lock(), vec![vec!["A".to_string()]]);

    m.registry.lock().set_limits(PoolLimits::single(3));
    m.subscribe(vec![sub("C", FeedMode::Ltp)]).await.unwrap();
    assert_eq!(
        *told.lock(),
        vec![vec!["A".to_string()], vec!["C".to_string()]]
    );
}

/// What a waited release did not let go of is told held again, and what
/// nobody holds any more is not.
#[tokio::test]
async fn a_release_not_let_go_of_is_told_held_again() {
    let m = WebSocketManager::with_config(fast());
    let told: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    m.admit(vec![sub("A", FeedMode::Ltp)]).await;
    m.set_held(Some(recorder(told.clone())));
    m.tell_let_go_answered(
        &[sub("A", FeedMode::Ltp), sub("B", FeedMode::Ltp)],
        &[Err("still subscribed".to_string()), Err("gone".to_string())],
    );
    assert_eq!(*told.lock(), vec![vec!["A".to_string()]]);
}

/// The session ending lets go of everything still held, so a key no feed
/// registered yet cannot outlive it; and the hooks go with it.
#[tokio::test]
async fn a_disconnect_lets_go_of_everything_held() {
    let m = WebSocketManager::with_config(fast());
    let told: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    m.set_let_go(Some(recorder(told.clone())));
    m.set_held(Some(recorder(Arc::default())));
    m.admit(vec![sub("A", FeedMode::Ltp)]).await;

    m.disconnect().await.unwrap();
    assert_eq!(*told.lock(), vec![vec!["A".to_string()]]);
    assert!(m.let_go.lock().is_none() && m.held.lock().is_none());
}
