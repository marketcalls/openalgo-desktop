//! A source key's owners and what the source answered them (audit C01,
//! C02; web `websocket_proxy/server.py` `subscribe_client` and
//! `_unsubscribe_owned_subscription`).
//!
//! * Subscribe: the first owner asks the source; owners that join before
//!   it answers wait on the same answer. A refusal is every waiting
//!   owner's, and each lets go (`settle_subscribe`) without asking the
//!   source to release what it never took.
//! * Unsubscribe: an owner other owners share the key with goes at once
//!   (no broker call). The last owner of an explicit unsubscribe stays
//!   while the source releases the key, and goes only when the broker let
//!   go (`finish_unsubscribe`), so a refused release can be asked again
//!   and is released once. An owner joining meanwhile asks the source to
//!   hold the key again.
//! * One release is one operation (review R107-01, R107-02): it carries a
//!   ticket, and only the answer to the latest release settles the owner,
//!   whenever it comes (a reply deadline is not an answer). The owner
//!   asking again while its release is with the source (a duplicate item
//!   in one batch, a retry after the deadline) joins that release's
//!   answer, never a shared owner's removal and never a success before
//!   the broker answered (review B1); the client subscribing the key again
//!   meanwhile asks the source to hold it, and the release then leaves it
//!   in place.
//! * Who may decide a release is who holds the key now, not who once did
//!   (review B1): an owner that joins during a release counts while it
//!   holds on, and is forgotten when its subscribe fails or it leaves.
//! * A disconnect (`Registry::remove_client`) drops its owners whatever
//!   the source answers; a release it starts is the source's to finish.

use super::{source_depth, ClientId, Inner, Registry, SourceKey};
use crate::feed::source::{answered, InstrumentKey, Mode, SourceOp, SourceOutcome};
use futures_util::future::{FutureExt, Shared};
use std::collections::HashSet;

/// The source's answer to an operation, which everyone waiting on that
/// operation shares.
pub type Answer = Shared<SourceOp>;

/// The last owner's release while it is with the source.
struct Releasing {
    /// The owner it is for.
    owner: ClientId,
    /// Which release it is (`Release::Waiting`).
    ticket: u64,
    answer: Answer,
}

/// One source key's owners.
pub(super) struct SourceRef {
    owners: usize,
    /// The source's answer to the subscribe that holds the key now.
    admission: Answer,
    /// The last owner's release, while it is with the source.
    releasing: Option<Releasing>,
    /// The owners that joined while that release was with the source, the
    /// first of whom asked the source to hold the key again. Only those
    /// that hold on count (review B1).
    joined: HashSet<ClientId>,
    /// The releasing owner itself subscribed the key again meanwhile, and
    /// asked the source to hold it: the release leaves it in place.
    reclaimed: bool,
}

impl SourceRef {
    fn refused(&self) -> bool {
        matches!(self.admission.peek(), Some(Err(_)))
    }
}

/// What a subscribe waits on before its acknowledgement, and what it
/// moves from.
pub struct Admission {
    answer: SourceOp,
    replacing: Option<u8>,
}

impl Admission {
    pub(super) fn new(answer: SourceOp, replacing: Option<u8>) -> Self {
        Self { answer, replacing }
    }

    /// The depth the subscription moves from, if it moves.
    pub fn replacing(&self) -> Option<u8> {
        self.replacing
    }

    /// The source's answer.
    pub async fn answer(self) -> SourceOutcome {
        self.answer.await
    }
}

/// What an unsubscribe waits on.
pub enum Release {
    /// Done: the client held nothing, or other owners keep the key.
    Done,
    /// The source is releasing the key for its last owner; its answer,
    /// with the release's ticket, goes to `finish_unsubscribe`, however
    /// late it comes.
    Waiting { answer: Answer, ticket: u64 },
}

impl Registry {
    /// One more owner of `(key, mode, depth)`, and the source's answer to
    /// wait on: asked now for a key nobody holds, for one whose release is
    /// with the source, or for one the source refused (it may take it
    /// now); else the answer the first owner waits on.
    pub(super) fn acquire(
        &self,
        inner: &mut Inner,
        id: ClientId,
        key: &InstrumentKey,
        mode: Mode,
        depth: u8,
    ) -> SourceOp {
        let sk: SourceKey = (key.clone(), mode, source_depth(mode, depth));
        let ask = || self.source.subscribe(key, mode, sk.2).shared();
        match inner.refcounts.get_mut(&sk) {
            None => {
                let admission = ask();
                inner.refcounts.insert(
                    sk,
                    SourceRef {
                        owners: 1,
                        admission: admission.clone(),
                        releasing: None,
                        joined: HashSet::new(),
                        reclaimed: false,
                    },
                );
                Box::pin(admission)
            }
            Some(r) => {
                r.owners += 1;
                let joining = r.releasing.is_some() && !r.reclaimed;
                let first = joining && r.joined.is_empty();
                if joining {
                    r.joined.insert(id);
                }
                if first || r.refused() {
                    r.admission = ask();
                }
                Box::pin(r.admission.clone())
            }
        }
    }

    /// The answer for a client subscribing again a key it holds: the one it
    /// holds, unless its own release of the key is with the source (a
    /// retry after the reply deadline). Then the source is asked to hold the
    /// key again, and the release leaves the owner in place when it comes
    /// (review R107-02); a release the broker has refused already is over.
    pub(super) fn own_admission(
        &self,
        inner: &mut Inner,
        key: &InstrumentKey,
        mode: Mode,
        depth: u8,
    ) -> Admission {
        let sk: SourceKey = (key.clone(), mode, source_depth(mode, depth));
        let Some(r) = inner.refcounts.get_mut(&sk) else {
            return Admission::new(answered(Ok(())), None);
        };
        if let Some(rel) = &r.releasing {
            if !r.reclaimed && r.joined.is_empty() {
                if matches!(rel.answer.peek(), Some(Err(_))) {
                    r.releasing = None;
                } else {
                    r.reclaimed = true;
                    r.admission = self.source.subscribe(key, mode, sk.2).shared();
                }
            }
        }
        Admission::new(Box::pin(r.admission.clone()), None)
    }

    /// Client `id` no longer owns `(key, mode, depth)`. The last owner asks
    /// the source to release the key, unless the source never took it or a
    /// release is with it already; the source is not waited on. The last
    /// joiner of a release leaving (its subscribe failed, or it went) retires
    /// the join: the key is held for nobody but the releasing owner, whose
    /// release goes on, so the source is told it need not hold it.
    pub(super) fn drop_owner(
        &self,
        inner: &mut Inner,
        id: ClientId,
        key: &InstrumentKey,
        mode: Mode,
        depth: u8,
    ) {
        let sk: SourceKey = (key.clone(), mode, source_depth(mode, depth));
        let Some(r) = inner.refcounts.get_mut(&sk) else {
            tracing::error!(
                "Feed refcount missing for {}:{} {:?}",
                key.exchange,
                key.symbol,
                mode
            );
            return;
        };
        r.owners = r.owners.saturating_sub(1);
        let had_joiners = !r.joined.is_empty();
        let joiner = r.joined.remove(&id);
        if r.owners > 0 {
            if joiner && r.joined.is_empty() && r.releasing.is_some() {
                r.admission = answered(Ok(())).shared();
                drop(self.source.unsubscribe(key, mode, sk.2));
            }
            return;
        }
        let Some(r) = inner.refcounts.remove(&sk) else {
            return;
        };
        let settled_by_source = if r.releasing.is_some() {
            !had_joiners && !r.reclaimed
        } else {
            r.refused()
        };
        if !settled_by_source {
            drop(self.source.unsubscribe(key, mode, sk.2));
        }
    }

    /// The source answered client `id`'s subscribe of `key` at `mode`:
    /// taken, so the depth it moved from goes; refused, so the client lets
    /// go of the new one and keeps the old depth if it had one. Nothing
    /// when the client left meanwhile (`remove_client` dropped both).
    pub fn settle_subscribe(
        &self,
        id: ClientId,
        key: &InstrumentKey,
        mode: Mode,
        depth: u8,
        replacing: Option<u8>,
        outcome: &SourceOutcome,
    ) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let map_key = (key.clone(), mode);
        let Some(held) = inner
            .clients
            .get_mut(&id)
            .and_then(|c| c.subs.get_mut(&map_key))
        else {
            return;
        };
        if held.depth != depth || held.replacing != replacing {
            return;
        }
        match (outcome, replacing) {
            (Ok(()), None) => {}
            (Ok(()), Some(old)) => {
                held.replacing = None;
                self.drop_owner(inner, id, key, mode, old);
            }
            (Err(_), Some(old)) => {
                held.depth = old;
                held.replacing = None;
                self.drop_owner(inner, id, key, mode, depth);
                Self::set_slot(inner, id, key, mode, Some(old));
            }
            (Err(_), None) => {
                if let Some(c) = inner.clients.get_mut(&id) {
                    c.subs.remove(&map_key);
                }
                self.drop_owner(inner, id, key, mode, depth);
                Self::set_slot(inner, id, key, mode, None);
            }
        }
    }

    /// Start releasing client `id`'s `key` at `mode`: done at once when it
    /// holds nothing (a no-op success, as on the web) or other owners keep
    /// the key; else the client stays its owner while the source releases.
    pub fn begin_unsubscribe(&self, id: ClientId, key: &InstrumentKey, mode: Mode) -> Release {
        let mut guard = self.inner.lock();
        self.begin(&mut guard, id, key, mode)
    }

    /// `begin_unsubscribe` for everything client `id` holds, in
    /// subscription order.
    pub fn begin_unsubscribe_all(&self, id: ClientId) -> Vec<(InstrumentKey, Mode, Release)> {
        let mut guard = self.inner.lock();
        let mut held: Vec<_> = match guard.clients.get(&id) {
            Some(c) => c.subs.iter().map(|(k, h)| (k.clone(), h.seq)).collect(),
            None => return Vec::new(),
        };
        held.sort_by_key(|(_, seq)| *seq);
        held.into_iter()
            .map(|((key, mode), _)| {
                let release = self.begin(&mut guard, id, &key, mode);
                (key, mode, release)
            })
            .collect()
    }

    fn begin(&self, inner: &mut Inner, id: ClientId, key: &InstrumentKey, mode: Mode) -> Release {
        let map_key = (key.clone(), mode);
        let Some(held) = inner
            .clients
            .get(&id)
            .and_then(|c| c.subs.get(&map_key))
            .copied()
        else {
            return Release::Done;
        };
        let sk: SourceKey = (key.clone(), mode, source_depth(mode, held.depth));
        inner.seq += 1;
        let ticket = inner.seq;
        let Some(r) = inner.refcounts.get_mut(&sk) else {
            self.forget(inner, id, key, mode, held.depth);
            return Release::Done;
        };
        // Its own release is with the source (a duplicate item in one
        // batch, a retry after the reply deadline): this is the same
        // operation, and it hears the same answer (review B1).
        if let Some(rel) = r
            .releasing
            .as_ref()
            .filter(|rel| rel.owner == id && !r.reclaimed)
        {
            return Release::Waiting {
                answer: rel.answer.clone(),
                ticket: rel.ticket,
            };
        }
        let last = r.owners == 1 && r.joined.is_empty() && (r.releasing.is_some() || !r.refused());
        if !last {
            self.forget(inner, id, key, mode, held.depth);
            return Release::Done;
        }
        let answer = self.source.unsubscribe(key, mode, sk.2).shared();
        r.reclaimed = false;
        r.releasing = Some(Releasing {
            owner: id,
            ticket,
            answer: answer.clone(),
        });
        Release::Waiting { answer, ticket }
    }

    /// Client `id`'s `key` at `mode` is no longer its.
    fn forget(&self, inner: &mut Inner, id: ClientId, key: &InstrumentKey, mode: Mode, depth: u8) {
        if let Some(c) = inner.clients.get_mut(&id) {
            c.subs.remove(&(key.clone(), mode));
        }
        self.drop_owner(inner, id, key, mode, depth);
        Self::set_slot(inner, id, key, mode, None);
    }

    /// The source answered release `ticket` of client `id`'s `key` at
    /// `mode`: the client goes if the broker let go, and stays its owner
    /// (to ask again) if not, or if it subscribed the key again meanwhile.
    /// Nothing when the client left meanwhile, or for an answer to another
    /// release than the one with the source now.
    pub fn finish_unsubscribe(
        &self,
        id: ClientId,
        key: &InstrumentKey,
        mode: Mode,
        ticket: u64,
        outcome: &SourceOutcome,
    ) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let Some(held) = inner
            .clients
            .get(&id)
            .and_then(|c| c.subs.get(&(key.clone(), mode)))
            .copied()
        else {
            return;
        };
        let sk: SourceKey = (key.clone(), mode, source_depth(mode, held.depth));
        let Some(r) = inner.refcounts.get_mut(&sk) else {
            return;
        };
        if r.releasing.as_ref().map(|rel| rel.ticket) != Some(ticket) {
            return;
        }
        r.releasing = None;
        let rejoined = !std::mem::take(&mut r.joined).is_empty();
        let reclaimed = std::mem::take(&mut r.reclaimed);
        if reclaimed {
            return;
        }
        if outcome.is_err() {
            // This owner holds it still: the source is told, as only here
            // is it known that someone does (review B1).
            self.source.keep(key, mode, sk.2);
            return;
        }
        if rejoined {
            // The joining owner's subscribe holds the key again; this
            // owner's release is done.
            self.forget(inner, id, key, mode, held.depth);
            return;
        }
        if let Some(c) = inner.clients.get_mut(&id) {
            c.subs.remove(&(key.clone(), mode));
        }
        inner.refcounts.remove(&sk);
        Self::set_slot(inner, id, key, mode, None);
    }
}

#[cfg(test)]
mod tests;
