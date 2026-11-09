//! An admission the manager waits on (review R107-03). Where a broker
//! answers subscribes apart from the socket (XTS over REST,
//! `BrokerFeed::admission_frames`), a subscribe is not taken until it
//! answered: the instruments it refused are let go, and their owners are
//! told why, instead of an acknowledgement for something that never
//! streams. A broker that answers on the socket has taken a subscribe once
//! the socket took its frames, as before.
//!
//! What is not a refusal: a connection that is not up (its next session
//! subscribes the registry whole), a socket that dropped the frames, an
//! answer that never came (the broker's worker went with its connection),
//! or one later than `ADMISSION_WAIT` (logged; the broker's worker keeps
//! trying).

use super::registry::{by_conn, Routed};
use super::release::Written;
use super::WebSocketManager;
use crate::brokers::common::streaming::{FeedSubscription, ReleaseAnswer};
use std::collections::HashMap;
use std::time::Duration;

/// Longest an admission waits for the broker's own answer.
pub(super) const ADMISSION_WAIT: Duration = Duration::from_secs(20);

/// The instruments a broker refused to stream, each with why.
pub(super) type Refusals = HashMap<(String, String), String>;

impl WebSocketManager {
    /// Hand each connection its share of an admission's changes, made when
    /// the registry's change count was `seq`, and wait, where it is up, for
    /// what its broker refused. A further connection that is not open yet
    /// is opened (its first session subscribes what it carries).
    pub(super) async fn apply_admitted(&self, (deltas, seq): (Vec<Routed>, u64)) -> Refusals {
        let mut refused = Refusals::new();
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
                refused.extend(refusals(later).await);
            }
        }
        self.close_idle().await;
        refused
    }

    /// Let go of the references the broker refused in an admission, which
    /// it never took: nothing goes to the broker for them.
    pub(super) fn let_go_refused(&self, placed: &[Option<FeedSubscription>], refused: &Refusals) {
        let mut reg = self.registry.lock();
        for s in placed.iter().flatten() {
            if refused.contains_key(&s.instrument_key()) {
                drop(reg.remove(s));
            }
        }
    }
}

/// What the brokers answered, within `ADMISSION_WAIT` each.
async fn refusals(later: Vec<(Vec<FeedSubscription>, ReleaseAnswer)>) -> Refusals {
    let mut out = Refusals::new();
    for (subs, answer) in later {
        match tokio::time::timeout(ADMISSION_WAIT, answer).await {
            Ok(Ok(list)) => {
                for (s, why) in list {
                    out.insert(s.instrument_key(), why);
                }
            }
            Ok(Err(_)) => {}
            Err(_) => tracing::warn!(
                instruments = subs.len(),
                "The broker did not answer a subscribe in {:?}; taken as streaming",
                ADMISSION_WAIT
            ),
        }
    }
    out
}
