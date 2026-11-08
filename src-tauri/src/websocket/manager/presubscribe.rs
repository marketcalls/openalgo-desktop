//! A broker's step before a new instrument is subscribed
//! (`BrokerFeed`-independent, set by the broker session with
//! `WebSocketManager::set_presubscribe`): Samco streams an index under a
//! listing id its master does not carry, and the web looks it up over REST
//! as it subscribes (bk-c RB-05). The manager awaits the step for the
//! instruments no reference holds yet, within a deadline and with no lock
//! held, before it places them; each failure is that item's refusal, in
//! the broker's words. Without a step nothing changes.

use super::WebSocketManager;
use crate::brokers::common::streaming::{FeedSubscription, Presubscribe};
use std::time::Duration;

/// Longest the manager waits for a broker's step.
pub(super) const PRESUBSCRIBE_DEADLINE: Duration = Duration::from_secs(10);

/// What an item answers when the broker's step did not answer in time.
pub(super) const PRESUBSCRIBE_LATE: &str =
    "The broker did not answer in time to start streaming this instrument. Subscribe again.";

/// What an item answers when the broker's step answered for other
/// instruments than it was asked (a defect, logged).
const PRESUBSCRIBE_UNANSWERED: &str =
    "The broker could not start streaming this instrument. Subscribe again.";

/// Each subscription as the step prepared it, only the broker's fields
/// changed: the instrument, mode and depth stay what the caller asked.
type Prepared = std::result::Result<FeedSubscription, String>;

impl WebSocketManager {
    /// The broker session's step before a new instrument is subscribed, or
    /// none (the default; a disconnect clears it).
    pub fn set_presubscribe(&self, step: Option<Presubscribe>) {
        *self.presubscribe.lock() = step;
    }

    /// `subs` as the feed will stream them: an instrument a reference holds
    /// already as it was placed; a new one through the broker's step, if
    /// there is one, awaited within the deadline with no lock held.
    pub(super) async fn presubscribed(&self, subs: Vec<FeedSubscription>) -> Vec<Prepared> {
        let step = self.presubscribe.lock().clone();
        let Some(step) = step else {
            return subs.into_iter().map(Ok).collect();
        };
        let fresh: Vec<usize> = {
            let reg = self.registry.lock();
            (0..subs.len())
                .filter(|i| !reg.entries.contains_key(&subs[*i].instrument_key()))
                .collect()
        };
        if fresh.is_empty() {
            return subs.into_iter().map(Ok).collect();
        }
        let asked: Vec<FeedSubscription> = fresh.iter().map(|i| subs[*i].clone()).collect();
        let answers = match tokio::time::timeout(PRESUBSCRIBE_DEADLINE, step(asked)).await {
            Ok(answers) if answers.len() == fresh.len() => answers,
            Ok(_) => {
                tracing::error!("The feed's subscribe step answered for other instruments");
                vec![Err(PRESUBSCRIBE_UNANSWERED.to_string()); fresh.len()]
            }
            Err(_) => vec![Err(PRESUBSCRIBE_LATE.to_string()); fresh.len()],
        };
        let mut answers = fresh.into_iter().zip(answers).peekable();
        let mut out = Vec::with_capacity(subs.len());
        for (i, asked) in subs.into_iter().enumerate() {
            let answer = answers.next_if(|(j, _)| *j == i).map(|(_, a)| a);
            out.push(match answer {
                None => Ok(asked),
                Some(Ok(prepared)) => Ok(FeedSubscription {
                    token: prepared.token,
                    brsymbol: prepared.brsymbol,
                    brexchange: prepared.brexchange,
                    ..asked
                }),
                Some(Err(why)) => Err(why),
            });
        }
        out
    }
}
