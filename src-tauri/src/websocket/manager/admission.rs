//! An admission the manager waits on (review R107-03, B2). Where a broker
//! answers subscribes apart from the socket (XTS over REST,
//! `BrokerFeed::admission_frames`), a subscribe is not taken until it
//! answered: the instruments it refused are let go, and their owners are
//! told why, instead of an acknowledgement for something that never
//! streams. A broker that answers on the socket has taken a subscribe once
//! the socket took its frames, as before.
//!
//! A subscribe the socket has not taken yet (the session is not up, or its
//! whole set carries the change) waits for the broker's answer to that
//! whole set instead (`await_set`), and one whose answer went with its
//! connection for the next session's. What has not been answered when
//! `ADMISSION_WAIT` is over is not a success either (review X1): it is
//! unsettled, so it is answered as not started and released, and the
//! broker is told, should it still take it. A connection that was never
//! started (the feed is not connected yet) takes what comes first.

use super::registry::{by_conn, Delta, Routed};
use super::release::Written;
use super::WebSocketManager;
use crate::brokers::common::streaming::{FeedSubscription, ReleaseAnswer, ReleaseRefusals};
use futures_util::future::Shared;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

/// Longest an admission waits for the broker's own answer: the largest XTS
/// admission (60 calls of 50 instruments, 0.5 s apart) and the search of
/// its refused batches, with room to spare.
pub(super) const ADMISSION_WAIT: Duration = Duration::from_secs(45);

/// What an instrument the broker did not answer in time is told.
pub(super) const ADMISSION_NOT_CONFIRMED: &str = "The broker did not confirm this subscription \
in time, so it was not started. Subscribe again.";

/// The instruments a broker did not take, each with why.
pub(super) type Refusals = HashMap<(String, String), String>;

/// What became of an admission's instruments the broker did not take.
#[derive(Default)]
pub(super) struct NotTaken {
    /// Every one of them, with why.
    pub(super) refusals: Refusals,
    /// Those it did not answer in time: it may yet take them.
    unsettled: HashSet<(String, String)>,
}

/// The broker's answer to a session's whole set, for whoever waits on it.
pub(super) type SetAnswer = Shared<ReleaseAnswer>;

/// The whole set a connection's session last sent.
#[derive(Clone)]
pub(super) struct SentSet {
    /// The registry's change count when it was read: it carries every
    /// change up to this one.
    pub(super) seq: u64,
    /// Which of the connection's sets it is.
    pub(super) generation: u64,
    /// Where the broker answers it; `None` when there was nothing to send.
    pub(super) answer: Option<SetAnswer>,
}

/// Where a connection's sets are announced.
pub(super) type Sets = watch::Receiver<Option<SentSet>>;

/// What a change asks the broker to take.
fn to_take(deltas: &[Delta]) -> Vec<FeedSubscription> {
    deltas
        .iter()
        .filter_map(|d| match d {
            Delta::Subscribe(s) | Delta::ModeChange(_, s) => Some(s.clone()),
            Delta::Unsubscribe(_) => None,
        })
        .collect()
}

impl WebSocketManager {
    /// Hand each connection its share of an admission's changes, made when
    /// the registry's change count was `seq`, and wait for what its broker
    /// refused, for a broker that says (`await_set` for a connection not
    /// up yet). A further connection that is not open yet is opened (its
    /// first session subscribes what it carries).
    pub(super) async fn apply_admitted(&self, (deltas, seq): (Vec<Routed>, u64)) -> NotTaken {
        let mut not_taken = NotTaken::default();
        let until = Instant::now() + ADMISSION_WAIT;
        for (conn, deltas) in by_conn(deltas) {
            let sets = self
                .connections
                .lock()
                .get(conn)
                .and_then(Option::as_ref)
                .map(|r| r.sets.clone());
            let Some(sets) = sets else {
                if conn > 0 {
                    self.open_extra(conn);
                }
                continue;
            };
            let subs = to_take(&deltas);
            match self.send_waited(conn, (deltas, seq), Vec::new()).await {
                Some(Written::Sent(later)) => {
                    settle(later, sets, seq, until, &mut not_taken).await;
                }
                _ => {
                    if let Some(sets) = sets {
                        await_set(sets, seq, subs, until, &mut not_taken).await;
                    }
                }
            }
        }
        self.close_idle().await;
        not_taken
    }

    /// Let go of the references the broker did not take. What it refused it
    /// never took, so nothing goes to it for them; what it did not answer
    /// in time it may yet take, so it is told to let go.
    pub(super) async fn let_go_refused(
        &self,
        placed: &[Option<FeedSubscription>],
        not_taken: &NotTaken,
    ) {
        let mut unsettled = Vec::new();
        let changes = {
            let mut reg = self.registry.lock();
            for s in placed.iter().flatten() {
                let key = s.instrument_key();
                if not_taken.refusals.contains_key(&key) {
                    let gone = reg.remove(s);
                    if not_taken.unsettled.contains(&key) {
                        unsettled.extend(gone);
                    }
                }
            }
            reg.changes
        };
        self.apply((unsettled, changes)).await;
    }
}

/// What the broker answered each change, by `until`. One whose answer went
/// with its connection is left to the next session's set.
async fn settle(
    later: Vec<(Vec<FeedSubscription>, ReleaseAnswer)>,
    sets: Option<Sets>,
    seq: u64,
    until: Instant,
    not_taken: &mut NotTaken,
) {
    for (subs, answer) in later {
        match tokio::time::timeout_at(until, answer).await {
            Ok(Ok(list)) => record(&subs, list, not_taken),
            Ok(Err(_)) => {
                if let Some(sets) = sets.clone() {
                    await_set(sets, seq, subs, until, not_taken).await;
                }
            }
            Err(_) => not_settled(subs, not_taken),
        }
    }
}

/// `subs` are in the whole set of a session (the first with `seq` read, then
/// the next if that one's answer goes with its connection): the broker's
/// answer to it decides them. Not answered by `until`: not settled.
async fn await_set(
    mut sets: Sets,
    seq: u64,
    subs: Vec<FeedSubscription>,
    until: Instant,
    not_taken: &mut NotTaken,
) {
    let mut answered = None;
    loop {
        let carried = |s: &Option<SentSet>| {
            s.as_ref().is_some_and(|s| {
                s.seq >= seq && answered.is_none_or(|generation| s.generation > generation)
            })
        };
        let set = tokio::time::timeout_at(until, sets.wait_for(carried))
            .await
            .map(|seen| seen.map(|set| set.clone()));
        let Ok(Ok(Some(set))) = set else { break };
        let Some(answer) = set.answer else { return };
        match tokio::time::timeout_at(until, answer).await {
            Ok(Ok(list)) => return record(&subs, list, not_taken),
            Ok(Err(_)) => answered = Some(set.generation),
            Err(_) => break,
        }
    }
    not_settled(subs, not_taken);
}

/// Of the broker's refusals, those that answer `subs`.
fn record(subs: &[FeedSubscription], list: ReleaseRefusals, not_taken: &mut NotTaken) {
    for (s, why) in list {
        let key = s.instrument_key();
        if subs.iter().any(|m| m.instrument_key() == key) {
            not_taken.refusals.insert(key, why);
        }
    }
}

/// The broker has not answered `subs` in time.
fn not_settled(subs: Vec<FeedSubscription>, not_taken: &mut NotTaken) {
    tracing::warn!(
        instruments = subs.len(),
        "The broker did not answer a subscribe in {:?}; not started",
        ADMISSION_WAIT
    );
    for s in subs {
        let key = s.instrument_key();
        not_taken
            .refusals
            .insert(key.clone(), ADMISSION_NOT_CONFIRMED.to_string());
        not_taken.unsettled.insert(key);
    }
}
