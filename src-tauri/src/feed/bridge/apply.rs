//! How the bridge brings the managers in line with what feed clients want:
//! a desired key becomes a manager reference on the main feed, or on the
//! broker's depth socket for the deep book plus a quote on the main feed,
//! and a reference nobody wants any more is released.
//!
//! Feed clients wait on the outcome (audit C01, C02): a subscribe hears
//! whether the managers took its key, an unsubscribe whether the broker let
//! go of it ([`Ops`]). A release the broker refuses with nobody waiting (a
//! client that disconnected) is tried again with a doubling pause; a key
//! refused with nobody waiting (re-applied after a new broker session) is
//! tried again at the next pass and stays listed as refused until then.

use super::{deep, mode_of, BrokerBridge, DesiredKey, State};
use crate::brokers::common::streaming::{FeedMode, FeedSubscription};
use crate::feed::source::{Mode, SourceOutcome, DEFAULT_DEPTH};
use crate::websocket::{FeedStatus, WebSocketManager};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::Instant;

/// First pause before a release nobody waits on is tried again; doubled
/// per attempt up to `RETRY_MAX`.
const RETRY_BASE: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// What one desired key holds on the managers.
#[derive(Debug, Clone)]
pub(super) enum Applied {
    Main(FeedSubscription),
    /// Depth socket book plus the main feed's quote for the same instrument.
    Deep {
        depth: FeedSubscription,
        quote: FeedSubscription,
    },
}

impl Applied {
    pub(super) fn main_subs(&self) -> Vec<FeedSubscription> {
        vec![self.main_sub()]
    }

    /// Its reference on the main manager.
    fn main_sub(&self) -> FeedSubscription {
        match self {
            Applied::Main(s) => s.clone(),
            Applied::Deep { quote, .. } => quote.clone(),
        }
    }
}

type Waiters = Vec<oneshot::Sender<SourceOutcome>>;

/// Who waits on the bridge's operations, and what it retries.
#[derive(Default)]
pub(super) struct Ops {
    /// Feed clients waiting to hear whether a key was taken.
    admitting: HashMap<DesiredKey, Waiters>,
    /// Feed clients waiting to hear whether the broker let go of a key.
    releasing: HashMap<DesiredKey, Waiters>,
    /// Keys whose release is with the managers now.
    in_flight: HashSet<DesiredKey>,
    /// Keys the managers refused while no client waited, with why.
    pub(super) refused: HashMap<DesiredKey, String>,
    /// Releases the broker refused while no client waited: when to try
    /// again, and how many times it was tried.
    retry: HashMap<DesiredKey, (Instant, u32)>,
}

fn any_live(waiters: &Waiters) -> bool {
    waiters.iter().any(|w| !w.is_closed())
}

fn answer(waiters: Waiters, outcome: &SourceOutcome) {
    for w in waiters {
        let _ = w.send(outcome.clone());
    }
}

impl Ops {
    /// A feed client waits on whether `key` is taken.
    pub(super) fn wait_admission(&mut self, key: DesiredKey) -> oneshot::Receiver<SourceOutcome> {
        let (tx, rx) = oneshot::channel();
        self.admitting.entry(key).or_default().push(tx);
        rx
    }

    /// A feed client waits on whether the broker lets go of `key`.
    pub(super) fn wait_release(&mut self, key: DesiredKey) -> oneshot::Receiver<SourceOutcome> {
        self.retry.remove(&key);
        let (tx, rx) = oneshot::channel();
        self.releasing.entry(key).or_default().push(tx);
        rx
    }

    /// `key` is wanted again before its release started: the release is
    /// done for the owner that asked (another holds the key now).
    pub(super) fn wanted_again(&mut self, key: &DesiredKey) {
        self.refused.remove(key);
        if !self.in_flight.contains(key) {
            if let Some(w) = self.releasing.remove(key) {
                answer(w, &Ok(()));
            }
        }
    }

    /// Nobody wants `key` any more: a pending admission answers no one.
    pub(super) fn unwanted(&mut self, key: &DesiredKey, why: &str) {
        self.refused.remove(key);
        if let Some(w) = self.admitting.remove(key) {
            answer(w, &Err(why.to_string()));
        }
    }

    pub(super) fn releasing(&self, key: &DesiredKey) -> bool {
        self.in_flight.contains(key)
    }

    /// A key's references move between the managers (a silent deep book
    /// falling back): a release asked for meanwhile waits for it.
    pub(super) fn start_moving(&mut self, key: &DesiredKey) {
        self.in_flight.insert(key.clone());
    }

    pub(super) fn stop_moving(&mut self, key: &DesiredKey) {
        self.in_flight.remove(key);
    }

    /// When the next release retry is due.
    pub(super) fn next_retry(&self) -> Option<Instant> {
        self.retry.values().map(|(at, _)| *at).min()
    }

    /// The managers forgot every reference (a new broker session): no
    /// release is left to retry.
    pub(super) fn forget_retries(&mut self) {
        self.retry.clear();
    }

    /// Forget every waiter (they hear the bridge stopped) and retry.
    pub(super) fn clear(&mut self) {
        *self = Ops::default();
    }
}

/// A subscription the managers refused while no app waited on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    pub symbol: String,
    pub exchange: String,
    pub mode: &'static str,
    pub reason: String,
}

/// What the bridge's subscriptions stand at now (audit Phase 3.5): taken
/// and not yet streaming, released and not yet confirmed, releases the
/// broker refused that are being tried again, and keys refused while no
/// app waited.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Operations {
    pub waiting: usize,
    pub releasing: usize,
    pub release_retrying: usize,
    pub refused: Vec<Refused>,
}

impl State {
    pub(super) fn operations(&self) -> Operations {
        let mut refused: Vec<Refused> = self
            .ops
            .refused
            .iter()
            .map(|((k, mode, _), why)| Refused {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                mode: mode.label(),
                reason: why.clone(),
            })
            .collect();
        refused.sort_by(|a, b| (&a.exchange, &a.symbol).cmp(&(&b.exchange, &b.symbol)));
        Operations {
            waiting: self
                .desired
                .iter()
                .filter(|k| !self.applied.contains_key(*k) && !self.ops.refused.contains_key(*k))
                .count(),
            releasing: self.ops.in_flight.len(),
            release_retrying: self.ops.retry.len(),
            refused,
        }
    }
}

/// What a key's release came to.
struct Released {
    key: DesiredKey,
    applied: Applied,
    outcome: SourceOutcome,
}

impl BrokerBridge {
    /// Drop manager references held for `applied` entries, waiting for
    /// nothing (the bridge is stopping).
    pub(super) async fn release(&self, applied: Vec<Applied>) {
        let mut main = Vec::new();
        let mut deep = Vec::new();
        for a in applied {
            match a {
                Applied::Main(s) => main.push(s),
                Applied::Deep { depth, quote } => {
                    deep.push(depth);
                    main.push(quote);
                }
            }
        }
        if !main.is_empty() {
            if let Err(e) = self.manager.unsubscribe(main).await {
                tracing::warn!("Could not release market data subscriptions: {}", e);
            }
        }
        if !deep.is_empty() {
            if let Err(e) = self.depth_manager.unsubscribe(deep).await {
                tracing::warn!("Could not release market depth subscriptions: {}", e);
            }
        }
    }

    /// How a desired key is applied, or why it cannot be: at the depth
    /// socket's levels, on that socket with a quote on the main feed while
    /// it can stream them, else on the main feed's levels (`deep`).
    fn applied_for(&self, key: &DesiredKey) -> Result<Applied, String> {
        let sub = self
            .subscription(key)
            .ok_or_else(|| format!("Token not found for {} on {}", key.0.symbol, key.0.exchange))?;
        let deep = *self.deep.lock();
        let down = *self.deep_down.lock();
        if key.1 != Mode::Depth || (deep != Some(key.2) && down != Some(key.2)) {
            return Ok(Applied::Main(sub));
        }
        let refused = matches!(self.depth_manager.status(), FeedStatus::AuthFailed { .. });
        let fell_back = self.state.lock().deep_books.fell_back.contains(key);
        if deep != Some(key.2) || refused || fell_back {
            return Ok(Applied::Main(deep::on_main_levels(sub)));
        }
        Ok(Applied::Deep {
            quote: FeedSubscription {
                mode: FeedMode::Quote,
                depth: DEFAULT_DEPTH,
                ..sub.clone()
            },
            depth: sub,
        })
    }

    fn subscription(&self, key: &DesiredKey) -> Option<FeedSubscription> {
        let (k, mode, depth) = key;
        let row = self.symbols.by_symbol(&k.exchange, &k.symbol)?;
        Some(FeedSubscription {
            symbol: k.symbol.clone(),
            exchange: k.exchange.clone(),
            token: row.token.clone(),
            brsymbol: row.br_symbol().to_string(),
            brexchange: row.br_exchange().to_string(),
            mode: mode_of(*mode),
            depth: *depth,
        })
    }

    /// Bring the managers in line with the desired set: take what is
    /// wanted, release what is not, and answer whoever waits.
    pub(super) async fn reconcile(&self) {
        self.fall_back_silent().await;
        let (generation, add, remove) = self.plan();
        if !add.is_empty() {
            self.admit(generation, add).await;
        }
        if !remove.is_empty() {
            self.release_waited(generation, remove).await;
        }
        self.answer_held();
    }

    /// What this pass takes and releases (the releases leave `applied`
    /// for `in_flight`), and the session it runs in.
    fn plan(&self) -> (u64, Vec<DesiredKey>, Vec<(DesiredKey, Applied)>) {
        let mut st = self.state.lock();
        let now = Instant::now();
        let State {
            desired,
            applied,
            last_tick,
            ops,
            generation,
            deep_books,
        } = &mut *st;
        let mut add: Vec<DesiredKey> = desired
            .iter()
            .filter(|k| !applied.contains_key(*k) && !ops.in_flight.contains(*k))
            .cloned()
            .collect();
        desired.in_order(&mut add);
        let gone: Vec<DesiredKey> = applied
            .keys()
            .filter(|k| !desired.contains(k))
            .filter(|k| ops.retry.get(*k).is_none_or(|(at, _)| *at <= now))
            .cloned()
            .collect();
        let remove = gone
            .into_iter()
            .filter_map(|k| {
                let a = applied.remove(&k)?;
                ops.in_flight.insert(k.clone());
                Some((k, a))
            })
            .collect();
        last_tick.retain(|k, _| desired.holds(k));
        // A retry is for a reference still held that nobody wants; one the
        // managers forgot (a new broker session) or that is wanted again
        // has nothing to retry, and must not wake the loop.
        ops.retry
            .retain(|k, _| applied.contains_key(k) && !desired.contains(k));
        deep_books.retain(|k| desired.contains(k));
        (*generation, add, remove)
    }

    /// Ask a manager to take these references: one answer each, so the
    /// members of a batch that fit start and the rest are told why (C02).
    async fn take(
        &self,
        manager: &WebSocketManager,
        subs: Vec<FeedSubscription>,
    ) -> Vec<SourceOutcome> {
        if subs.is_empty() {
            return Vec::new();
        }
        manager.admit(subs).await
    }

    /// Take `keys` on the managers: the quote on the main feed first, then
    /// the deep book of a deep key (its quote goes again when the book is
    /// refused).
    async fn admit(&self, generation: u64, keys: Vec<DesiredKey>) {
        let mut answers: Vec<(DesiredKey, SourceOutcome, Option<Applied>)> = Vec::new();
        let mut routed = Vec::new();
        for k in keys {
            match self.applied_for(&k) {
                Ok(a) => routed.push((k, a)),
                Err(why) => answers.push((k, Err(why), None)),
            }
        }
        let main = routed.iter().map(|(_, a)| a.main_sub()).collect();
        let mut outcomes = self.take(&self.manager, main).await;
        let deep: Vec<(usize, FeedSubscription)> = routed
            .iter()
            .enumerate()
            .filter_map(|(i, (_, a))| match a {
                Applied::Deep { depth, .. } if outcomes[i].is_ok() => Some((i, depth.clone())),
                _ => None,
            })
            .collect();
        let books = deep.iter().map(|(_, s)| s.clone()).collect();
        let deep_outcomes = self.take(&self.depth_manager, books).await;
        let mut quotes_back = Vec::new();
        for ((i, _), outcome) in deep.iter().zip(deep_outcomes) {
            if outcome.is_err() {
                quotes_back.push(routed[*i].1.main_sub());
                outcomes[*i] = outcome;
            }
        }
        if !quotes_back.is_empty() {
            let _ = self.manager.unsubscribe(quotes_back).await;
        }
        for ((k, a), outcome) in routed.into_iter().zip(outcomes) {
            let taken = outcome.is_ok().then_some(a);
            answers.push((k, outcome, taken));
        }
        self.settle_admissions(generation, answers);
    }

    /// Record what the managers took and answer whoever waited. A refusal
    /// a client waits on lets go of the key (its owners let go too); one
    /// nobody waits on keeps it wanted and listed as refused.
    fn settle_admissions(
        &self,
        generation: u64,
        answers: Vec<(DesiredKey, SourceOutcome, Option<Applied>)>,
    ) {
        let mut st = self.state.lock();
        let current = st.generation == generation;
        let mut again = false;
        for (k, outcome, taken) in answers {
            let waiters = st.ops.admitting.remove(&k).unwrap_or_default();
            match (&outcome, taken) {
                (Ok(()), Some(a)) if current => {
                    again |= !st.desired.contains(&k);
                    if matches!(a, Applied::Deep { .. }) {
                        st.deep_books.watch(&k);
                    }
                    st.applied.insert(k.clone(), a);
                    st.ops.refused.remove(&k);
                }
                (Ok(()), _) => {}
                (Err(why), _) => {
                    if any_live(&waiters) {
                        st.desired.remove(&k);
                    } else if st.desired.contains(&k) {
                        tracing::warn!(
                            "{} on {} could not be streamed: {}",
                            k.0.symbol,
                            k.0.exchange,
                            why
                        );
                        st.ops.refused.insert(k.clone(), why.clone());
                    }
                }
            }
            answer(waiters, &outcome);
        }
        drop(st);
        if again {
            self.changed.notify_one();
        }
    }

    /// Release keys and wait for the broker: a deep book first, then the
    /// main reference (the quote of a deep key only once its book went;
    /// a book that went before a refused quote is taken again).
    async fn release_waited(&self, generation: u64, remove: Vec<(DesiredKey, Applied)>) {
        let books: Vec<FeedSubscription> = remove
            .iter()
            .filter_map(|(_, a)| match a {
                Applied::Deep { depth, .. } => Some(depth.clone()),
                Applied::Main(_) => None,
            })
            .collect();
        let mut book_outcomes = self.depth_manager.release(books).await.into_iter();
        let mut outcomes: Vec<SourceOutcome> = remove
            .iter()
            .map(|(_, a)| match a {
                Applied::Deep { .. } => book_outcomes.next().unwrap_or(Ok(())),
                Applied::Main(_) => Ok(()),
            })
            .collect();
        let main: Vec<(usize, FeedSubscription)> = remove
            .iter()
            .enumerate()
            .filter(|(i, _)| outcomes[*i].is_ok())
            .map(|(i, (_, a))| (i, a.main_sub()))
            .collect();
        let main_outcomes = self
            .manager
            .release(main.iter().map(|(_, s)| s.clone()).collect())
            .await;
        let mut books_back = Vec::new();
        for ((i, _), outcome) in main.iter().zip(main_outcomes) {
            if outcome.is_err() {
                if let Applied::Deep { depth, .. } = &remove[*i].1 {
                    books_back.push(depth.clone());
                }
                outcomes[*i] = outcome;
            }
        }
        if !books_back.is_empty() {
            let _ = self.depth_manager.subscribe(books_back).await;
        }
        let released = remove
            .into_iter()
            .zip(outcomes)
            .map(|((key, applied), outcome)| Released {
                key,
                applied,
                outcome,
            })
            .collect();
        self.settle_releases(generation, released);
    }

    /// Record what the broker let go of and answer whoever waited. What it
    /// kept stays applied: wanted again when its client waits (the client
    /// keeps the subscription), else tried again after a pause.
    fn settle_releases(&self, generation: u64, released: Vec<Released>) {
        let mut st = self.state.lock();
        let current = st.generation == generation;
        let now = Instant::now();
        let mut again = false;
        for r in released {
            st.ops.in_flight.remove(&r.key);
            let waiters = st.ops.releasing.remove(&r.key).unwrap_or_default();
            if let (Err(why), true) = (&r.outcome, current) {
                if any_live(&waiters) {
                    st.desired.insert(r.key.clone());
                    st.ops.retry.remove(&r.key);
                } else if !st.desired.contains(&r.key) {
                    let attempt = st.ops.retry.get(&r.key).map_or(0, |(_, n)| n + 1);
                    let pause = RETRY_BASE
                        .saturating_mul(1u32 << attempt.min(6))
                        .min(RETRY_MAX);
                    tracing::warn!(
                        "The broker still streams {} on {}; trying again in {:?}: {}",
                        r.key.0.symbol,
                        r.key.0.exchange,
                        pause,
                        why
                    );
                    st.ops.retry.insert(r.key.clone(), (now + pause, attempt));
                }
                st.applied.insert(r.key.clone(), r.applied);
            } else {
                st.ops.retry.remove(&r.key);
            }
            again |= st.desired.contains(&r.key) && !st.applied.contains_key(&r.key);
            let outcome = if current { r.outcome } else { Ok(()) };
            answer(waiters, &outcome);
        }
        drop(st);
        if again {
            self.changed.notify_one();
        }
    }

    /// Answer the clients waiting on a key that is held now (it came back
    /// when its release was refused, or was wanted again meanwhile).
    fn answer_held(&self) {
        let mut st = self.state.lock();
        let State {
            desired,
            applied,
            ops,
            ..
        } = &mut *st;
        let held: Vec<DesiredKey> = ops
            .admitting
            .keys()
            .filter(|k| {
                desired.contains(k) && applied.contains_key(*k) && !ops.in_flight.contains(*k)
            })
            .cloned()
            .collect();
        for k in held {
            if let Some(w) = ops.admitting.remove(&k) {
                answer(w, &Ok(()));
            }
        }
    }
}

#[cfg(test)]
mod tests;
