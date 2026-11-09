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
//!   deadline and with no lock held, before it places them;
//! * what it hears when an instrument is placed (`set_held`) and when no
//!   consumer holds one any more (`set_let_go`), whether or not a socket
//!   is up to carry it.
//!
//! Each refusal or failure is that item's answer. Without any of them
//! nothing changes.

use super::release::ReleaseOutcome;
use super::WebSocketManager;
use crate::brokers::common::streaming::{
    FeedHeld, FeedLetGo, FeedRefusal, FeedSubscription, Presubscribe,
};
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

    /// What the broker session hears when an instrument is let go of, or
    /// nothing (the default).
    pub fn set_let_go(&self, let_go: Option<FeedLetGo>) {
        *self.let_go.lock() = let_go;
    }

    /// What the broker session hears when an instrument is placed, or
    /// nothing (the default).
    pub fn set_held(&self, held: Option<FeedHeld>) {
        *self.held.lock() = held;
    }

    /// The broker session hears which of `subs` a consumer holds now. Read
    /// under the registry lock, as `tell_let_go`, so that whichever order
    /// a placement and a release run in, it hears what is true last.
    pub(super) fn tell_held<'a>(&self, subs: impl IntoIterator<Item = &'a FeedSubscription>) {
        let Some(held) = self.held.lock().clone() else {
            return;
        };
        let reg = self.registry.lock();
        let holding: Vec<FeedSubscription> = subs
            .into_iter()
            .filter(|s| reg.entries.contains_key(&s.instrument_key()))
            .cloned()
            .collect();
        if !holding.is_empty() {
            held(&holding);
        }
    }

    /// The broker session hears which of `subs` no consumer holds now.
    /// Read under the registry lock, so an instrument placed again keeps
    /// what it holds.
    pub(super) fn tell_let_go(&self, subs: &[FeedSubscription]) {
        let Some(let_go) = self.let_go.lock().clone() else {
            return;
        };
        let reg = self.registry.lock();
        let gone: Vec<FeedSubscription> = subs
            .iter()
            .filter(|s| !reg.entries.contains_key(&s.instrument_key()))
            .cloned()
            .collect();
        if !gone.is_empty() {
            let_go(&gone);
        }
    }

    /// What a waited release leaves the session knowing: what was let go
    /// of is told so. What the socket did not take or the broker refused
    /// stays held, and is told so again: the feed may have freed its key
    /// as it wrote the frames that then failed.
    pub(super) fn tell_let_go_answered(
        &self,
        subs: &[FeedSubscription],
        answers: &[ReleaseOutcome],
    ) {
        let done: Vec<FeedSubscription> = subs
            .iter()
            .zip(answers)
            .filter(|(_, answer)| answer.is_ok())
            .map(|(s, _)| s.clone())
            .collect();
        self.tell_let_go(&done);
        self.tell_held(
            subs.iter()
                .zip(answers)
                .filter(|(_, answer)| answer.is_err())
                .map(|(s, _)| s),
        );
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
