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
//! * A mode downgrade a release left (`downgrade_frames`, review B3), the
//!   mirror: the lower code is taken first, the higher released only once
//!   it is, and the release is waited on through both.
//! * A session's whole set (`Kind::Sync`, review B2): what the broker
//!   refuses goes back too, for the manager to let go and the trader to be
//!   told, and one refused instrument does not stop the others.
//!
//! A passing failure of an admission is sent again with its waiter, and
//! only what that retry comes to answers it: a retry report is not an
//! answer (review B2). If every attempt fails its owner is told it was not
//! started, and the broker to let go of it in case only its answers were
//! lost.

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

/// The same for a session's whole set, which nobody is waiting on in
/// front of a screen: what it leaves unasked stays subscribed but silent
/// until the next reconnect (review B2).
const SYNC_SEARCH_TIME: Duration = Duration::from_secs(120);

/// An admission's instrument the broker never answered about, however
/// often it was asked.
const NOT_ANSWERED: &str = "The broker did not answer the request to stream this instrument, \
so it was not started. Subscribe again.";

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

/// What the worker's answer is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A release its caller waits on (a mode downgrade included).
    Release,
    /// A subscribe its caller waits on.
    Admission,
    /// A session's whole set.
    Sync,
}

/// One waited release, shared by the commands it sent (one per message
/// code). When the last command has run its answer goes to the manager; a
/// release whose commands never ran (the worker went with its connection)
/// drops the sender, which the manager reads as "not confirmed".
#[derive(Clone)]
pub struct Waited(Arc<Mutex<Answer>>);

struct Answer {
    kind: Kind,
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

impl Answer {
    /// One command of it is done with: the answer goes once none is left.
    fn command_done(&mut self) {
        self.commands_left = self.commands_left.saturating_sub(1);
        if self.commands_left == 0 {
            if let Some(to) = self.to.take() {
                let _ = to.send(std::mem::take(&mut self.refused));
            }
        }
    }

    fn refuse(&mut self, instrument: &Value, why: String) {
        if let Some((_, s)) = self.instruments.iter().find(|(i, _)| i == instrument) {
            let s = s.clone();
            self.refused.push((s, why));
        }
    }
}

impl Waited {
    /// Whether it is an admission or a session's whole set (its passing
    /// failures are sent again).
    pub(super) fn admits(&self) -> bool {
        self.0.lock().kind != Kind::Release
    }

    /// When an admission stops searching a refused batch for what the
    /// broker refuses; `None` for a release, which is never searched.
    pub(super) fn search_until(&self) -> Option<Instant> {
        self.0.lock().search_until
    }

    /// A command of it ran: what the broker refused (and, for a release,
    /// did not answer) joins the answer, sent once every command is done
    /// with. `more`: this command is not (what it could not settle goes
    /// again, or a further step follows it). A refused token settles nothing
    /// (the feed reconnects): an admission is left to the next session's
    /// set, so its answer is dropped.
    pub(super) fn ran(&self, report: &CommandReport, more: bool) {
        let mut a = self.0.lock();
        if report.reconnect && a.kind != Kind::Release {
            a.to = None;
            return;
        }
        let failed: Vec<(Value, String)> = if a.kind == Kind::Admission {
            report
                .refused
                .iter()
                .map(|(i, why)| (i.clone(), refused_to_stream(why)))
                .collect()
        } else if a.kind == Kind::Sync {
            // What the search had no time for is not known to be refused.
            report
                .refused
                .iter()
                .filter(|(_, why)| why != NOT_CHECKED)
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
            a.refuse(&inst, why);
        }
        if !more {
            a.command_done();
        }
    }

    /// Asked every time the worker asks, the broker never answered about
    /// `instruments`: an admission is told so; a session's whole set just
    /// tries again at the next reconnect. True when they are now nobody's,
    /// so the broker is to let go of them in case it took them.
    pub(super) fn gave_up(&self, instruments: &[Value]) -> bool {
        let mut a = self.0.lock();
        let told = a.kind == Kind::Admission;
        if told {
            for inst in instruments {
                a.refuse(inst, NOT_ANSWERED.to_string());
            }
        }
        a.command_done();
        told
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

/// The release or the subscribe of `subs`, by `kind`, as REST commands
/// the worker answers: no socket frames, and the receiver of the broker's
/// answer.
pub(super) fn frames(
    feed: &mut XtsFeed,
    subs: &[FeedSubscription],
    kind: Kind,
) -> (Vec<Message>, Option<ReleaseAnswer>) {
    let subscribe = kind != Kind::Release;
    if subscribe {
        feed.remember(subs);
    } else {
        feed.forget(subs);
    }
    let commands = feed.commands(subs, subscribe);
    (Vec::new(), send_waited(feed, subs, commands, kind))
}

/// Mode changes (old, new) that take a new message code before they let go
/// of the old one: the new code is asked first and `Command::then_release`
/// releases the old once it is taken; a change that keeps its code is the
/// plain mode change (nothing to refuse). `kind` is who waits.
fn taken_then_released(
    feed: &mut XtsFeed,
    changes: &[(FeedSubscription, FeedSubscription)],
    kind: Kind,
) -> (Vec<Message>, Option<ReleaseAnswer>) {
    let mut frames = Vec::new();
    let mut changed = Vec::new();
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
        changed.push(new.clone());
        match moves.iter_mut().find(|(codes, _)| *codes == (to, from)) {
            Some((_, v)) => v.push(inst),
            None => moves.push(((to, from), vec![inst])),
        }
    }
    feed.remember(&changed);
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
    let answer = send_waited(feed, &changed, commands, kind);
    (frames, answer)
}

/// A waited admission's mode upgrades (old, new): a refusal leaves each
/// instrument streaming as it was (review X2).
pub(super) fn upgrade_frames(
    feed: &mut XtsFeed,
    changes: &[(FeedSubscription, FeedSubscription)],
) -> (Vec<Message>, Option<ReleaseAnswer>) {
    taken_then_released(feed, changes, Kind::Admission)
}

/// A waited release's mode downgrades (old, new): the lower code is taken
/// before the higher goes, so the lower holders never lose their stream;
/// what the broker refuses, either way, is that release's answer (review
/// B3).
pub(super) fn downgrade_frames(
    feed: &mut XtsFeed,
    changes: &[(FeedSubscription, FeedSubscription)],
) -> (Vec<Message>, Option<ReleaseAnswer>) {
    taken_then_released(feed, changes, Kind::Release)
}

/// Hand the worker `commands`, one waited admission or release of `subs`
/// between them: where the broker's answer comes, if there is anything to
/// send.
fn send_waited(
    feed: &mut XtsFeed,
    subs: &[FeedSubscription],
    mut commands: Vec<Command>,
    kind: Kind,
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
    let search_time = match kind {
        Kind::Release => None,
        Kind::Admission => Some(SEARCH_TIME),
        Kind::Sync => Some(SYNC_SEARCH_TIME),
    };
    let (to, answer) = oneshot::channel();
    let waited = Waited(Arc::new(Mutex::new(Answer {
        kind,
        search_until: search_time.map(|t| Instant::now() + its_calls + t),
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
