//! What the broker session decides before a subscription is placed, set
//! with the session's feed and cleared at disconnect, so every consumer of
//! the manager (feed clients through the bridge, Flow, the sandbox,
//! strategies) hears the same answer:
//!
//! * its refusal (`set_refusal`): a mode, depth or exchange the broker's
//!   web adapter refuses, in its words (IndMoney refuses Depth);
//! * its step before a new instrument (`set_presubscribe`): Samco streams
//!   an index under a listing id its master does not carry, and the web
//!   looks it up over REST as it subscribes (bk-c RB-05). The manager
//!   awaits it for the instruments no reference holds yet, within a
//!   deadline and with no lock held, before it places them.
//!
//! Each refusal or failure is that item's answer. Without either nothing
//! changes.

use super::WebSocketManager;
use crate::brokers::common::streaming::{FeedRefusal, FeedSubscription, Presubscribe};
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
    /// The broker session's refusals, or none (the default).
    pub fn set_refusal(&self, refusal: Option<FeedRefusal>) {
        *self.refusal.lock() = refusal;
    }

    /// The broker session's step before a new instrument is subscribed, or
    /// none (the default).
    pub fn set_presubscribe(&self, step: Option<Presubscribe>) {
        *self.presubscribe.lock() = step;
    }

    /// `subs` as the feed will stream them, or why not: what the broker
    /// refuses is refused; an instrument a reference holds already goes as
    /// it was placed; a new one through the broker's step, if there is
    /// one, awaited within the deadline with no lock held.
    pub(super) async fn presubscribed(&self, subs: Vec<FeedSubscription>) -> Vec<Prepared> {
        let refusal = self.refusal.lock().clone();
        let mut out: Vec<Prepared> = subs
            .into_iter()
            .map(|s| match refusal.as_ref().and_then(|refuse| refuse(&s)) {
                Some(why) => Err(why),
                None => Ok(s),
            })
            .collect();
        let step = self.presubscribe.lock().clone();
        let Some(step) = step else {
            return out;
        };
        let fresh: Vec<usize> = {
            let reg = self.registry.lock();
            (0..out.len())
                .filter(|i| {
                    out[*i]
                        .as_ref()
                        .is_ok_and(|s| !reg.entries.contains_key(&s.instrument_key()))
                })
                .collect()
        };
        if fresh.is_empty() {
            return out;
        }
        let asked: Vec<FeedSubscription> = fresh
            .iter()
            .filter_map(|i| out[*i].as_ref().ok().cloned())
            .collect();
        let answers = match tokio::time::timeout(PRESUBSCRIBE_DEADLINE, step(asked)).await {
            Ok(answers) if answers.len() == fresh.len() => answers,
            Ok(_) => {
                tracing::error!("The feed's subscribe step answered for other instruments");
                vec![Err(PRESUBSCRIBE_UNANSWERED.to_string()); fresh.len()]
            }
            Err(_) => vec![Err(PRESUBSCRIBE_LATE.to_string()); fresh.len()],
        };
        for (i, answer) in fresh.into_iter().zip(answers) {
            let prepared = match answer {
                Ok(p) => match &out[i] {
                    Ok(asked) => Ok(FeedSubscription {
                        token: p.token,
                        brsymbol: p.brsymbol,
                        brexchange: p.brexchange,
                        ..asked.clone()
                    }),
                    Err(why) => Err(why.clone()),
                },
                Err(why) => Err(why),
            };
            out[i] = prepared;
        }
        out
    }
}
