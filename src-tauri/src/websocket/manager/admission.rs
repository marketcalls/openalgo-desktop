//! An admission the manager waits on (review R107-03). Where a broker
//! answers subscribes apart from the socket (XTS over REST,
//! `BrokerFeed::admission_frames`), a subscribe is not taken until it
//! answered: the instruments it refused are let go, and their owners are
//! told why, instead of an acknowledgement for something that never
//! streams. A broker that answers on the socket has taken a subscribe once
//! the socket took its frames, as before.
//!
//! What is not a refusal: a connection that is not up (its next session
//! subscribes the registry whole), a socket that dropped the frames, or an
//! answer that never came because the broker's worker went with its
//! connection. An answer later than `ADMISSION_WAIT` is not a success
//! either (review X1): the admission is unsettled, so it is answered as not
//! started and released, and the broker is told, should it still take it.

use super::registry::{by_conn, Routed};
use super::release::Written;
use super::WebSocketManager;
use crate::brokers::common::streaming::{FeedSubscription, ReleaseAnswer};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

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

impl WebSocketManager {
    /// Hand each connection its share of an admission's changes, made when
    /// the registry's change count was `seq`, and wait, where it is up, for
    /// what its broker refused. A further connection that is not open yet
    /// is opened (its first session subscribes what it carries).
    pub(super) async fn apply_admitted(&self, (deltas, seq): (Vec<Routed>, u64)) -> NotTaken {
        let mut not_taken = NotTaken::default();
        for (conn, deltas) in by_conn(deltas) {
            let running = self
                .connections
                .lock()
                .get(conn)
                .is_some_and(Option::is_some);
            if !running {
                if conn > 0 {
                    self.open_extra(conn);
                }
                continue;
            }
            if let Some(Written::Sent(later)) =
                self.send_waited(conn, (deltas, seq), Vec::new()).await
            {
                settle(later, &mut not_taken).await;
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

/// What the brokers answered, within `ADMISSION_WAIT` each.
async fn settle(later: Vec<(Vec<FeedSubscription>, ReleaseAnswer)>, not_taken: &mut NotTaken) {
    for (subs, answer) in later {
        match tokio::time::timeout(ADMISSION_WAIT, answer).await {
            Ok(Ok(list)) => {
                for (s, why) in list {
                    not_taken.refusals.insert(s.instrument_key(), why);
                }
            }
            Ok(Err(_)) => {}
            Err(_) => {
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
        }
    }
}
