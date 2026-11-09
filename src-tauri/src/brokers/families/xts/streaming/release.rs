//! A release or an admission the manager waits on (audit C01, review
//! R107-03; `BrokerFeed::release_frames`, `admission_frames`). XTS takes
//! and lets go of an instrument through a REST call, so the answer comes
//! from the subscription worker, not from the socket.
//!
//! * A release: the instruments the broker did not let go go back to the
//!   manager with why, and such a release is never retried in the
//!   background, since its caller keeps the subscription and asks again
//!   (web `_unsubscribe_owned_subscription`).
//! * An admission: the instruments the broker refused go back with why, so
//!   their owners are told and let go. A passing failure (no answer, a
//!   server error) or a refused token is not a refusal: the worker sends
//!   it again, and a new session subscribes it again. The web logs a
//!   refused subscribe and acknowledges it (a web defect: it never
//!   streams).
//! * A mode upgrade an admission brought (`upgrade_frames`, review X2): the
//!   new message code is taken first and the old one released only once
//!   it is, so a refusal leaves the instrument streaming as it was.

use super::{mapping, Command, CommandReport, XtsFeed, SUBSCRIBE_BATCH, SUBSCRIBE_GAP};
use crate::brokers::common::streaming::{
    BrokerFeed, FeedSubscription, Message, ReleaseAnswer, ReleaseRefusals,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::Instant;

/// Why a batch failed when the broker refused the market-data token (the
/// feed reconnects with a new one).
pub(super) const TOKEN_REFUSED: &str = "The broker ended the market data session before it \
stopped streaming this instrument, so it stays subscribed. Unsubscribe again once the feed \
reconnects.";

/// How long an admission may search a refused batch for the instruments
/// the broker refuses, on top of its own calls (review X1). What the
/// manager waits for (`ADMISSION_WAIT`) covers it with room to spare.
const SEARCH_TIME: Duration = Duration::from_secs(12);

/// An instrument of a refused batch the search had no time to ask about
/// on its own. The broker did refuse the batch that held it, so it is not
/// streaming.
pub(super) const NOT_CHECKED: &str = "The broker refused the group of instruments this one was \
sent in, and there was no time to check it on its own. Subscribe to it again.";

/// Why a batch failed when the broker did not answer.
const NO_ANSWER: &str = "The broker did not answer the request to stop streaming this \
instrument, so it stays subscribed. Unsubscribe again to retry.";

/// XTS's refusal to release an instrument it does not stream for this
/// session: nothing to let go, so the release is done.
pub fn is_not_subscribed(body: &str) -> bool {
    body.to_ascii_lowercase().contains("not subscribed")
}

/// What the broker answered a refused subscribe with, for the trader.
fn refused_to_stream(why: &str) -> String {
    if why == NOT_CHECKED {
        return why.to_string();
    }
    format!(
        "The broker refused to stream this instrument ({}).",
        why.trim_end_matches('.')
    )
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
    /// An admission (else a release).
    admission: bool,
    /// When an admission stops searching its refused batches.
    search_until: Option<Instant>,
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
    /// Whether it is an admission (its passing failures are sent again).
    pub(super) fn admits(&self) -> bool {
        self.0.lock().admission
    }

    /// When an admission stops searching a refused batch for what the
    /// broker refuses; `None` for a release, which is never searched.
    pub(super) fn search_until(&self) -> Option<Instant> {
        self.0.lock().search_until
    }

    /// One command of it ran: what the broker refused (and, for a release,
    /// did not answer) joins the answer, sent once every command has run.
    pub(super) fn ran(&self, report: &CommandReport) {
        let mut a = self.0.lock();
        let failed: Vec<(Value, String)> = if a.admission {
            report
                .refused
                .iter()
                .filter(|(_, why)| why != TOKEN_REFUSED)
                .map(|(i, why)| (i.clone(), refused_to_stream(why)))
                .collect()
        } else {
            report
                .refused
                .iter()
                .map(|(i, why)| (i.clone(), refused(why)))
                .chain(
                    report
                        .retry
                        .iter()
                        .map(|i| (i.clone(), NO_ANSWER.to_string())),
                )
                .collect()
        };
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

    /// Decode these instruments' events.
    pub(super) fn remember(&mut self, subs: &[FeedSubscription]) {
        for s in subs {
            if let Some(k) = Self::key(s) {
                self.book.insert(k, s.clone());
            }
        }
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

/// The release (or, with `admission`, the subscribe) of `subs` as REST
/// commands the worker answers: no socket frames, and the receiver of the
/// broker's answer.
pub(super) fn frames(
    feed: &mut XtsFeed,
    subs: &[FeedSubscription],
    admission: bool,
) -> (Vec<Message>, Option<ReleaseAnswer>) {
    if admission {
        feed.remember(subs);
    } else {
        feed.forget(subs);
    }
    let commands = feed.commands(subs, admission);
    (Vec::new(), send_waited(feed, subs, commands, admission))
}

/// A waited admission's mode upgrades (old, new) as REST commands: each
/// instrument is taken at its new message code and its old code goes only
/// once the broker took the new one (`Command::then_release`), so a refusal
/// leaves it streaming as it was (review X2). An upgrade that keeps its
/// code (a 5-level book to 20, on one code) is the plain mode change: the
/// code it streams at does not change and there is nothing to refuse.
pub(super) fn upgrade_frames(
    feed: &mut XtsFeed,
    changes: &[(FeedSubscription, FeedSubscription)],
) -> (Vec<Message>, Option<ReleaseAnswer>) {
    let mut frames = Vec::new();
    let mut upgraded = Vec::new();
    let mut moves: Vec<((u16, u16), Vec<Value>)> = Vec::new();
    for (old, new) in changes {
        let (Some((from, _)), Some((to, inst))) = (feed.instrument(old), feed.instrument(new))
        else {
            continue;
        };
        if from == to {
            frames.extend(feed.mode_change_frames(old, new));
            continue;
        }
        upgraded.push(new.clone());
        match moves.iter_mut().find(|(codes, _)| *codes == (to, from)) {
            Some((_, v)) => v.push(inst),
            None => moves.push(((to, from), vec![inst])),
        }
    }
    feed.remember(&upgraded);
    let commands = moves
        .into_iter()
        .map(|((code, from), instruments)| Command {
            subscribe: true,
            code,
            instruments,
            waited: None,
            then_release: Some(from),
        })
        .collect();
    let answer = send_waited(feed, &upgraded, commands, true);
    (frames, answer)
}

/// Hand the worker `commands`, one waited admission or release of `subs`
/// between them: where the broker's answer comes, if there is anything to
/// send.
fn send_waited(
    feed: &mut XtsFeed,
    subs: &[FeedSubscription],
    mut commands: Vec<Command>,
    admission: bool,
) -> Option<ReleaseAnswer> {
    if commands.is_empty() {
        return None;
    }
    let instruments = subs
        .iter()
        .filter_map(|s| feed.instrument(s).map(|(_, i)| (i, s.clone())))
        .collect();
    let calls: usize = commands
        .iter()
        .map(|c| c.instruments.len().div_ceil(SUBSCRIBE_BATCH))
        .sum();
    let its_calls = SUBSCRIBE_GAP.saturating_mul(u32::try_from(calls).unwrap_or(u32::MAX));
    let (to, answer) = oneshot::channel();
    let waited = Waited(Arc::new(Mutex::new(Answer {
        admission,
        search_until: admission.then(|| Instant::now() + its_calls + SEARCH_TIME),
        commands_left: commands.len(),
        instruments,
        refused: Vec::new(),
        to: Some(to),
    })));
    for c in &mut commands {
        c.waited = Some(waited.clone());
    }
    drop(waited);
    for c in commands {
        feed.enqueue(c);
    }
    Some(answer)
}

#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod tests;
