//! A release the manager waits on (audit C01, `BrokerFeed::release_frames`).
//! XTS lets go of an instrument through a REST call, so the answer comes
//! from the subscription worker, not from the socket: the instruments the
//! broker did not let go go back to the manager with why, and such a
//! release is never retried in the background, since its caller keeps the
//! subscription and asks again (web `_unsubscribe_owned_subscription`).

use super::{mapping, CommandReport, XtsFeed};
use crate::brokers::common::streaming::{
    FeedSubscription, Message, ReleaseAnswer, ReleaseRefusals,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::oneshot;

/// Why a batch failed when the broker refused the market-data token (the
/// feed reconnects with a new one).
pub(super) const TOKEN_REFUSED: &str = "The broker ended the market data session before it \
stopped streaming this instrument, so it stays subscribed. Unsubscribe again once the feed \
reconnects.";

/// Why a batch failed when the broker did not answer.
const NO_ANSWER: &str = "The broker did not answer the request to stop streaming this \
instrument, so it stays subscribed. Unsubscribe again to retry.";

/// XTS's refusal to release an instrument it does not stream for this
/// session: nothing to let go, so the release is done.
pub fn is_not_subscribed(body: &str) -> bool {
    body.to_ascii_lowercase().contains("not subscribed")
}

/// What the broker answered a refused release with, for the trader.
fn refused(why: &str) -> String {
    if why == TOKEN_REFUSED {
        return why.to_string();
    }
    format!(
        "The broker refused to stop streaming this instrument ({}), so it stays subscribed. \
Unsubscribe again to retry.",
        why.trim_end_matches('.')
    )
}

/// One waited release, shared by the commands it sent (one per message
/// code). When the last command has run its answer goes to the manager; a
/// release whose commands never ran (the worker went with its connection)
/// drops the sender, which the manager reads as "not confirmed".
#[derive(Clone)]
pub struct Waited(Arc<Mutex<Answer>>);

struct Answer {
    commands_left: usize,
    instruments: Vec<(Value, FeedSubscription)>,
    refused: ReleaseRefusals,
    to: Option<oneshot::Sender<ReleaseRefusals>>,
}

impl std::fmt::Debug for Waited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waited")
    }
}

impl PartialEq for Waited {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Waited {
    /// One command of the release ran: what the broker refused or did not
    /// answer joins the answer, sent once every command has run.
    pub(super) fn ran(&self, report: CommandReport) {
        let mut a = self.0.lock();
        let failed = report
            .refused
            .into_iter()
            .map(|(i, why)| (i, refused(&why)))
            .chain(report.retry.into_iter().map(|i| (i, NO_ANSWER.to_string())));
        for (inst, why) in failed {
            if let Some((_, s)) = a.instruments.iter().find(|(i, _)| *i == inst) {
                let s = s.clone();
                a.refused.push((s, why));
            }
        }
        a.commands_left = a.commands_left.saturating_sub(1);
        if a.commands_left == 0 {
            if let Some(to) = a.to.take() {
                let _ = to.send(std::mem::take(&mut a.refused));
            }
        }
    }
}

impl XtsFeed {
    /// The message code and the REST instrument of one subscription.
    pub(super) fn instrument(&self, s: &FeedSubscription) -> Option<(u16, Value)> {
        let (seg, token) = Self::key(s)?;
        let code = self.cfg.mode_code(s.mode.code());
        let inst =
            json!({"exchangeSegment": seg, "exchangeInstrumentID": mapping::instrument_id(&token)});
        Some((code, inst))
    }

    /// Stop decoding these instruments' events.
    pub(super) fn forget(&mut self, subs: &[FeedSubscription]) {
        for s in subs {
            if let Some(k) = Self::key(s) {
                self.book.remove(&k);
                self.sides.forget(&k);
            }
        }
    }
}

/// The release of `subs` as REST commands the worker answers: no socket
/// frames, and the receiver of the broker's answer.
pub(super) fn frames(
    feed: &mut XtsFeed,
    subs: &[FeedSubscription],
) -> (Vec<Message>, Option<ReleaseAnswer>) {
    feed.forget(subs);
    let mut commands = feed.commands(subs, false);
    if commands.is_empty() {
        return (Vec::new(), None);
    }
    let instruments = subs
        .iter()
        .filter_map(|s| feed.instrument(s).map(|(_, i)| (i, s.clone())))
        .collect();
    let (to, answer) = oneshot::channel();
    let waited = Waited(Arc::new(Mutex::new(Answer {
        commands_left: commands.len(),
        instruments,
        refused: Vec::new(),
        to: Some(to),
    })));
    for c in &mut commands {
        c.release = Some(waited.clone());
    }
    drop(waited);
    for c in commands {
        feed.enqueue(c);
    }
    (Vec::new(), Some(answer))
}

#[cfg(test)]
mod tests;
