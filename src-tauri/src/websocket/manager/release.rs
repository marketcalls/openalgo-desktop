//! A release a caller waits on (audit C01; web `websocket_proxy/server.py`
//! `_unsubscribe_owned_subscription`): the references go, and the caller
//! hears whether the broker let go of what no reference holds any more.
//! What the broker did not let go is put back, so the registry, the next
//! connection and the caller's retry agree.
//!
//! "Let go" is what the broker's protocol can tell: the frames reached the
//! socket, or the broker answered a release it handles apart from the
//! socket (XTS over REST, `BrokerFeed::release_frames`). With no socket
//! carrying subscriptions (connecting, reconnecting, refused), the
//! release is done: the next connection is sent the registry without it.

use super::{Apply, Command, Delta, WebSocketManager};
use crate::brokers::common::streaming::{FeedSubscription, ReleaseAnswer};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use tokio::sync::oneshot;

/// What a waited release answers when its socket dropped before taking it.
pub(super) const RELEASE_NOT_TAKEN: &str = "The connection to the broker dropped before it \
could stop streaming this instrument, so it stays subscribed. Unsubscribe again to retry.";

/// What a waited release answers when the broker never confirmed it.
pub(super) const RELEASE_NOT_CONFIRMED: &str = "The broker did not confirm that it stopped \
streaming this instrument, so it stays subscribed. Unsubscribe again to retry.";

/// What the socket did with a release its caller waits on.
pub(super) enum Written {
    /// Written. For a broker that answers releases itself, the
    /// instruments of each such release and where its answer comes.
    Sent(Vec<(Vec<FeedSubscription>, ReleaseAnswer)>),
    /// The socket did not take it: the session is replaced, and the
    /// references the release dropped are back in the registry, so the
    /// next connection subscribes them again.
    Failed,
}

/// One reference's answer: done, or why the broker still streams it.
pub type ReleaseOutcome = std::result::Result<(), String>;

impl WebSocketManager {
    /// Drop these references and wait for the broker to let go of what no
    /// reference holds any more (audit C01): one answer per reference, in
    /// order. A reference another consumer also holds goes at once, as
    /// the web drops an owner locally while others remain. What the socket
    /// did not take, or the broker refused, stays held and answers why.
    pub async fn release(&self, subs: Vec<FeedSubscription>) -> Vec<ReleaseOutcome> {
        let epoch = self.epoch.load(Ordering::SeqCst);
        let deltas: Vec<Delta> = {
            let mut reg = self.registry.lock();
            subs.iter().filter_map(|s| reg.remove(s)).collect()
        };
        let mut answers = vec![Ok(()); subs.len()];
        if deltas.is_empty() {
            return answers;
        }
        let refused = match self.send_waited(deltas, subs.clone()).await {
            None => return answers,
            Some(Written::Failed) => return vec![Err(RELEASE_NOT_TAKEN.to_string()); subs.len()],
            Some(Written::Sent(later)) => refusals(later).await,
        };
        if refused.is_empty() {
            return answers;
        }
        let mut back = Vec::new();
        for (answer, s) in answers.iter_mut().zip(&subs) {
            if let Some(why) = refused.get(&s.instrument_key()) {
                *answer = Err(why.clone());
                back.push(s.clone());
            }
        }
        self.put_back(back, epoch).await;
        answers
    }

    /// Hand the change to the socket's task and wait for what it did;
    /// `None` when no socket carried it (no task, or the task dropped it
    /// while connecting or reconnecting): the registry is the truth then.
    async fn send_waited(
        &self,
        deltas: Vec<Delta>,
        undo: Vec<FeedSubscription>,
    ) -> Option<Written> {
        let tx = self.running.lock().as_ref().map(|r| r.cmd.clone())?;
        let (reply, written) = oneshot::channel();
        tx.send(Command::Apply(Apply {
            deltas,
            reply: Some(reply),
            undo,
        }))
        .await
        .ok()?;
        written.await.ok()
    }

    /// Hold again the references a broker refused to release, unless every
    /// reference was forgotten since (a disconnect), and send them again:
    /// a broker that still streams them answers "already subscribed", and
    /// the feed learns them again (XTS keeps no book of what it released).
    async fn put_back(&self, subs: Vec<FeedSubscription>, epoch: u64) {
        let deltas: Vec<Delta> = {
            let mut reg = self.registry.lock();
            if self.epoch.load(Ordering::SeqCst) != epoch {
                return;
            }
            subs.into_iter().filter_map(|s| reg.add(s)).collect()
        };
        self.apply(deltas).await;
    }
}

/// Why each instrument a broker refused to release stays subscribed; an
/// answer that never came refuses every instrument of its release.
async fn refusals(
    later: Vec<(Vec<FeedSubscription>, ReleaseAnswer)>,
) -> HashMap<(String, String), String> {
    let mut out = HashMap::new();
    for (subs, answer) in later {
        match answer.await {
            Ok(list) => {
                for (s, why) in list {
                    out.insert(s.instrument_key(), why);
                }
            }
            Err(_) => {
                for s in subs {
                    out.insert(s.instrument_key(), RELEASE_NOT_CONFIRMED.to_string());
                }
            }
        }
    }
    out
}
