//! The broker's step before a new instrument is subscribed (bk-c RB-05,
//! Samco's cold index): awaited for new instruments only, its prepared
//! fields streamed, its refusals and a step past the deadline answered per
//! item, with no lock held while it runs.

use super::presubscribe::{PRESUBSCRIBE_DEADLINE, PRESUBSCRIBE_LATE};
use super::tests::fast;
use super::*;
use crate::brokers::common::streaming::{FeedMode, Presubscribe};
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
