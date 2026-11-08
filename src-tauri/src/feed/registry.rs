//! Who is subscribed to what, and how many clients hold each source key.
//!
//! One lock guards three maps that must change together:
//! * per client: its outbox, identity, and the `(instrument, mode)` keys it
//!   holds (with the depth it asked for);
//! * per instrument: the clients holding it and at which modes, which is
//!   what a tick fans out to;
//! * per source key `(instrument, mode, depth)`: a reference count. The
//!   source is told to subscribe on 0 -> 1 and to unsubscribe on 1 -> 0.
//!
//! Every entry a client creates is removed by `remove_client`, which the
//! connection's drop guard calls on every exit path (clean close, error,
//! abrupt disconnect, task abort), so nothing outlives the socket.

use super::outbox::Outbox;
use super::protocol;
use super::source::{InstrumentKey, MarketDataSource, MarketUpdate, Mode, DEFAULT_DEPTH};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

pub type ClientId = u64;

/// Per-mode depth a client holds for one instrument (`None` = not held).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ModeSlots([Option<u8>; 3]);

impl ModeSlots {
    fn get(&self, mode: Mode) -> Option<u8> {
        self.0[mode.index()]
    }
    fn set(&mut self, mode: Mode, depth: Option<u8>) {
        self.0[mode.index()] = depth;
    }
    fn is_empty(&self) -> bool {
        self.0.iter().all(Option::is_none)
    }
}

#[derive(Debug, Clone, Copy)]
struct Held {
    depth: u8,
    seq: u64,
}

struct ClientRec {
    outbox: Arc<Outbox>,
    user_id: Arc<str>,
    broker: Arc<str>,
    orders: bool,
    subs: HashMap<(InstrumentKey, Mode), Held>,
}

struct InstrumentRec {
    id: u64,
    holders: HashMap<ClientId, ModeSlots>,
}

type SourceKey = (InstrumentKey, Mode, u8);

#[derive(Default)]
struct Inner {
    clients: HashMap<ClientId, ClientRec>,
    instruments: HashMap<InstrumentKey, InstrumentRec>,
    refcounts: HashMap<SourceKey, usize>,
    next_instrument: u64,
    seq: u64,
}

/// Snapshot for tests and diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryStats {
    pub clients: usize,
    pub instruments: usize,
    pub source_keys: usize,
    pub client_subscriptions: usize,
    pub queued_frames: usize,
}

pub struct Registry {
    source: Arc<dyn MarketDataSource>,
    inner: Mutex<Inner>,
    max_subscriptions: usize,
}

fn source_depth(mode: Mode, depth: u8) -> u8 {
    if mode == Mode::Depth {
        depth
    } else {
        DEFAULT_DEPTH
    }
}

impl Registry {
    pub fn new(source: Arc<dyn MarketDataSource>, max_subscriptions: usize) -> Self {
        Self {
            source,
            inner: Mutex::new(Inner::default()),
            max_subscriptions: max_subscriptions.max(1),
        }
    }

    pub fn source(&self) -> &Arc<dyn MarketDataSource> {
        &self.source
    }

    pub fn add_client(&self, id: ClientId, outbox: Arc<Outbox>) {
        self.inner.lock().clients.insert(
            id,
            ClientRec {
                outbox,
                user_id: Arc::from(""),
                broker: Arc::from(""),
                orders: false,
                subs: HashMap::new(),
            },
        );
    }

    pub fn set_identity(&self, id: ClientId, user_id: &str, broker: &str) {
        if let Some(c) = self.inner.lock().clients.get_mut(&id) {
            c.user_id = Arc::from(user_id);
            c.broker = Arc::from(broker);
        }
    }

    pub fn set_orders(&self, id: ClientId, on: bool) {
        if let Some(c) = self.inner.lock().clients.get_mut(&id) {
            c.orders = on;
        }
    }

    fn acquire(&self, inner: &mut Inner, key: &InstrumentKey, mode: Mode, depth: u8) {
        let sk = (key.clone(), mode, source_depth(mode, depth));
        let n = inner.refcounts.entry(sk).or_insert(0);
        *n += 1;
        if *n == 1 {
            self.source.subscribe(key, mode, source_depth(mode, depth));
        }
    }

    fn release(&self, inner: &mut Inner, key: &InstrumentKey, mode: Mode, depth: u8) {
        let sk = (key.clone(), mode, source_depth(mode, depth));
        let last = match inner.refcounts.get_mut(&sk) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => true,
            None => {
                tracing::error!(
                    "Feed refcount missing for {}:{} {:?}",
                    key.exchange,
                    key.symbol,
                    mode
                );
                false
            }
        };
        if last {
            inner.refcounts.remove(&sk);
            self.source.unsubscribe(key, mode, sk.2);
        }
    }

    fn set_slot(inner: &mut Inner, id: ClientId, key: &InstrumentKey, mode: Mode, d: Option<u8>) {
        if let Some(depth) = d {
            let next = &mut inner.next_instrument;
            let rec = inner.instruments.entry(key.clone()).or_insert_with(|| {
                *next += 1;
                InstrumentRec {
                    id: *next,
                    holders: HashMap::new(),
                }
            });
            rec.holders.entry(id).or_default().set(mode, Some(depth));
        } else if let Some(rec) = inner.instruments.get_mut(key) {
            if let Some(slots) = rec.holders.get_mut(&id) {
                slots.set(mode, None);
                if slots.is_empty() {
                    rec.holders.remove(&id);
                }
            }
            if rec.holders.is_empty() {
                inner.instruments.remove(key);
            }
        }
    }

    /// Hold `key` at `mode` for client `id`. Subscribing again replaces the
    /// depth (the old source key is released). Errors carry the per-symbol
    /// message for the ack.
    pub fn subscribe(
        &self,
        id: ClientId,
        key: &InstrumentKey,
        mode: Mode,
        depth: u8,
    ) -> Result<(), String> {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.seq += 1;
        let seq = inner.seq;
        let Some(client) = inner.clients.get_mut(&id) else {
            return Err("Connection closed".into());
        };
        let map_key = (key.clone(), mode);
        let previous = client.subs.get(&map_key).copied();
        match previous {
            Some(h) if source_depth(mode, h.depth) == source_depth(mode, depth) => {
                if let Some(h) = client.subs.get_mut(&map_key) {
                    h.depth = depth;
                }
                Self::set_slot(inner, id, key, mode, Some(depth));
                return Ok(());
            }
            Some(_) => {}
            None => {
                if client.subs.len() >= self.max_subscriptions {
                    return Err(format!(
                        "Subscription limit of {} reached for this connection",
                        self.max_subscriptions
                    ));
                }
            }
        }
        client.subs.insert(map_key, Held { depth, seq });
        self.acquire(inner, key, mode, depth);
        if let Some(old) = previous {
            self.release(inner, key, mode, old.depth);
        }
        Self::set_slot(inner, id, key, mode, Some(depth));
        Ok(())
    }

    /// Release `key` at `mode` for client `id`; a key the client does not
    /// hold is a successful no-op, as on the web.
    pub fn unsubscribe(&self, id: ClientId, key: &InstrumentKey, mode: Mode) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let held = inner
            .clients
            .get_mut(&id)
            .and_then(|c| c.subs.remove(&(key.clone(), mode)));
        if let Some(h) = held {
            self.release(inner, key, mode, h.depth);
            Self::set_slot(inner, id, key, mode, None);
        }
    }

    /// Release everything client `id` holds; returns what was released in
    /// subscription order.
    pub fn unsubscribe_all(&self, id: ClientId) -> Vec<(InstrumentKey, Mode)> {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let mut held: Vec<((InstrumentKey, Mode), Held)> = match inner.clients.get_mut(&id) {
            Some(c) => c.subs.drain().collect(),
            None => return Vec::new(),
        };
        held.sort_by_key(|(_, h)| h.seq);
        let mut out = Vec::with_capacity(held.len());
        for ((key, mode), h) in held {
            self.release(inner, &key, mode, h.depth);
            Self::set_slot(inner, id, &key, mode, None);
            out.push((key, mode));
        }
        out
    }

    /// Forget client `id` and release every source key it held.
    pub fn remove_client(&self, id: ClientId) {
        self.unsubscribe_all(id);
        let mut inner = self.inner.lock();
        if let Some(c) = inner.clients.remove(&id) {
            c.outbox.finish();
        }
    }

    /// Fan an update out to every holder, one frame per held mode not above
    /// `update.mode`, highest mode first.
    pub fn dispatch(&self, update: &MarketUpdate) {
        let inner = self.inner.lock();
        let Some(rec) = inner.instruments.get(&update.key) else {
            return;
        };
        // Frames differ only by (mode, depth, broker); render each once.
        let mut rendered: Vec<(Mode, u8, Arc<str>, String)> = Vec::new();
        for (cid, slots) in &rec.holders {
            let Some(client) = inner.clients.get(cid) else {
                continue;
            };
            for mode in Mode::ALL.iter().rev().copied() {
                if !protocol::reaches(update, mode, &client.broker) {
                    continue;
                }
                let Some(depth) = slots.get(mode) else {
                    continue;
                };
                let depth = if mode == Mode::Depth { depth } else { 0 };
                let frame = match rendered
                    .iter()
                    .find(|(m, d, b, _)| *m == mode && *d == depth && **b == *client.broker)
                {
                    Some((_, _, _, f)) => f.clone(),
                    None => {
                        let f = protocol::market_data(update, mode, depth, &client.broker);
                        rendered.push((mode, depth, client.broker.clone(), f.clone()));
                        f
                    }
                };
                client.outbox.push_market((rec.id, mode.as_u8()), frame);
            }
        }
    }

    /// Send an order update to every client that asked for them, rendered
    /// once per distinct user id.
    pub fn broadcast_orders(&self, render: impl Fn(&str) -> String) -> usize {
        let inner = self.inner.lock();
        let mut rendered: Vec<(Arc<str>, String)> = Vec::new();
        let mut n = 0;
        for c in inner.clients.values().filter(|c| c.orders) {
            let frame = match rendered.iter().find(|(u, _)| *u == c.user_id) {
                Some((_, f)) => f.clone(),
                None => {
                    let f = render(&c.user_id);
                    rendered.push((c.user_id.clone(), f.clone()));
                    f
                }
            };
            c.outbox.push_control(frame);
            n += 1;
        }
        n
    }

    pub fn stats(&self) -> RegistryStats {
        let inner = self.inner.lock();
        RegistryStats {
            clients: inner.clients.len(),
            instruments: inner.instruments.len(),
            source_keys: inner.refcounts.len(),
            client_subscriptions: inner.clients.values().map(|c| c.subs.len()).sum(),
            queued_frames: inner.clients.values().map(|c| c.outbox.len()).sum(),
        }
    }

    /// Queued frames per client (slow-consumer diagnostics).
    pub fn queue_lengths(&self) -> Vec<usize> {
        self.inner
            .lock()
            .clients
            .values()
            .map(|c| c.outbox.len())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::source::{FakeSource, SourceCall};

    fn key(s: &str) -> InstrumentKey {
        InstrumentKey::new(s, "NSE")
    }

    #[test]
    fn refcount_subscribes_once_and_releases_on_last() {
        let src = FakeSource::new([key("A")]);
        let r = Registry::new(src.clone(), 10);
        for id in 1..=3 {
            r.add_client(id, Arc::new(Outbox::new(8, 8)));
            r.subscribe(id, &key("A"), Mode::Ltp, 5).unwrap();
            r.subscribe(id, &key("A"), Mode::Ltp, 5).unwrap();
        }
        assert_eq!(src.active_count(), 1);
        r.unsubscribe(1, &key("A"), Mode::Ltp);
        r.remove_client(2);
        assert_eq!(src.active_count(), 1);
        r.remove_client(3);
        assert_eq!(src.active_count(), 0);
        assert!(src.violations().is_empty());
        assert_eq!(
            src.calls(),
            vec![
                SourceCall::Subscribe(key("A"), Mode::Ltp, 5),
                SourceCall::Unsubscribe(key("A"), Mode::Ltp, 5)
            ]
        );
        r.remove_client(1);
        assert_eq!(r.stats(), RegistryStats::default());
    }

    #[test]
    fn depth_change_moves_the_source_key() {
        let src = FakeSource::new([key("A")]);
        let r = Registry::new(src.clone(), 10);
        r.add_client(1, Arc::new(Outbox::new(8, 8)));
        r.subscribe(1, &key("A"), Mode::Depth, 5).unwrap();
        r.subscribe(1, &key("A"), Mode::Depth, 20).unwrap();
        assert_eq!(src.active(), vec![(key("A"), Mode::Depth, 20)]);
        assert_eq!(r.unsubscribe_all(1), vec![(key("A"), Mode::Depth)]);
        assert_eq!(src.active_count(), 0);
    }

    #[test]
    fn per_client_limit() {
        let src = FakeSource::permissive();
        let r = Registry::new(src, 2);
        r.add_client(1, Arc::new(Outbox::new(8, 8)));
        r.subscribe(1, &key("A"), Mode::Ltp, 5).unwrap();
        r.subscribe(1, &key("B"), Mode::Ltp, 5).unwrap();
        assert!(r.subscribe(1, &key("C"), Mode::Ltp, 5).is_err());
        // Re-subscribing a held key is not a new subscription.
        r.subscribe(1, &key("A"), Mode::Ltp, 5).unwrap();
    }
}
