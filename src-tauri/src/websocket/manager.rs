//! Broker-agnostic market-data feed manager (outbound side).
//!
//! One supervisor task owns the broker socket. It awaits the feed's async
//! `prepare` (authorize call, socket token), connects with a timeout,
//! sends the feed's handshake, re-subscribes every registered instrument,
//! reads frames through the broker's `BrokerFeed::parse`, and publishes
//! normalised events on a bounded broadcast channel. On error, close, or a
//! stall (no frame for `stall_timeout`), it closes the old socket and
//! reconnects with capped exponential backoff and jitter. An authentication
//! refusal stops the loop until the trader logs in again. Protocol replies
//! a feed returns from `parse` (`FeedEvent::Reply`) are written back on the
//! same socket.
//!
//! Subscriptions are reference counted per instrument and mode. The broker
//! sees one subscription per instrument at its effective (highest) mode; a
//! registry entry is removed when its last reference goes, so the registry
//! is bounded by what clients currently hold. A feed started with
//! `connect_pool` spreads its instruments over several broker connections,
//! as the web's `ConnectionPool` does (`pool`, audit C07).
//!
//! Resource ownership: the spawned tasks are the supervisors, one per open
//! connection, each kept as a `JoinHandle` and aborted (after a graceful
//! stop request) on `disconnect`; an extra connection that carries nothing
//! any more is stopped. The command channels are bounded; the event channel
//! is a bounded broadcast whose slow receivers see `Lagged` and skip ahead.

use crate::brokers::common::streaming::{
    BrokerFeed, FeedLetGo, FeedRefusal, FeedSubscription, MarketEvent, Presubscribe, StopKind,
};
use crate::error::{AppError, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

mod admission;
mod heartbeat;
mod losses;
mod packet;
mod pool;
mod presubscribe;
mod registry;
mod release;
mod stats;
mod supervisor;
pub use losses::{Reader, SinceConnect};
use packet::PacketOut;
pub use packet::{MarketPacket, Packet, PacketKind, SessionId};
pub use pool::{FeedFactory, PoolLimits};
use registry::{by_conn, Delta, Registry, Routed};
use release::Written;
pub use stats::{Connection, DropCause, FeedStats, LastDrop};
#[cfg(test)]
use supervisor::SessionEnd;
use supervisor::Supervisor;

/// Capacity of the event fan-out channel.
pub const TICK_CHANNEL_CAP: usize = 4096;

/// Tunables (tests shrink the timings).
#[derive(Debug, Clone)]
pub struct FeedConfig {
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// No frame (data or heartbeat) for this long means the socket is dead.
    pub stall_timeout: Duration,
    pub connect_timeout: Duration,
    /// Longest one batch of frames may take to reach the socket (TIME-02).
    /// A peer that stops reading would otherwise hold this loop inside the
    /// send, where its own stall watchdog cannot run.
    pub write_timeout: Duration,
    /// A connection that lived this long resets the backoff.
    pub stable_after: Duration,
    /// Most instruments the one connection of `connect` may carry (a pool
    /// takes its own limits, `connect_pool`).
    pub max_instruments: usize,
    pub command_capacity: usize,
    pub event_capacity: usize,
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            stall_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(15),
            write_timeout: Duration::from_secs(10),
            stable_after: Duration::from_secs(30),
            max_instruments: 3000,
            command_capacity: 256,
            event_capacity: TICK_CHANNEL_CAP,
        }
    }
}

/// Feed state, for the UI and the feed server.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FeedStatus {
    Disconnected,
    Connecting {
        broker: String,
        attempt: u32,
    },
    Connected {
        broker: String,
    },
    Reconnecting {
        broker: String,
        attempt: u32,
        delay_ms: u64,
    },
    /// The broker refused the session; log in again to resume.
    AuthFailed {
        broker: String,
        message: String,
    },
    /// The broker ended the feed for a reason other than the login (too
    /// many connections on one login, audit P-12): it stays down until the
    /// next sign-in, and the message says what to do.
    Halted {
        broker: String,
        kind: StopKind,
        message: String,
    },
}

impl FeedStatus {
    /// Why this connection streams nothing until the next sign-in: the
    /// broker refused the login or ended the feed.
    pub(crate) fn refusal(&self) -> Option<&str> {
        match self {
            FeedStatus::AuthFailed { message, .. } | FeedStatus::Halted { message, .. } => {
                Some(message)
            }
            _ => None,
        }
    }
}

/// A change for the socket.
struct Apply {
    deltas: Vec<Delta>,
    /// The registry's change count once these deltas were made: a session
    /// sent its whole set at or after it already carries them.
    seq: u64,
    /// A caller waits to hear what the socket did with it (a release,
    /// audit C01); dropped unanswered when no socket carried it.
    reply: Option<oneshot::Sender<Written>>,
    /// The references the change dropped, put back in the registry when
    /// the socket does not take it, so the next connection sends them.
    undo: Vec<FeedSubscription>,
}

impl Apply {
    fn plain(deltas: Vec<Delta>, seq: u64) -> Self {
        Self {
            deltas,
            seq,
            reply: None,
            undo: Vec::new(),
        }
    }
}

enum Command {
    Apply(Apply),
    /// Drop the socket and connect again (after the computer slept).
    Reconnect,
    Stop,
}

/// One connection's socket task, and its state.
struct Running {
    cmd: mpsc::Sender<Command>,
    task: JoinHandle<()>,
    status: watch::Receiver<FeedStatus>,
}

pub struct WebSocketManager {
    events: broadcast::Sender<MarketEvent>,
    /// The same events, one broker message per packet, and each
    /// connection's sessions (`packet`).
    packets: PacketOut,
    /// The first connection's state: the feed's, for the UI and the feed
    /// server.
    status: watch::Sender<FeedStatus>,
    registry: Arc<Mutex<Registry>>,
    /// The broker connections, first one first; `None` where an extra one
    /// is not open.
    connections: Mutex<Vec<Option<Running>>>,
    /// Makes the adapter for another connection of the session
    /// (`connect_pool`); `None` for a feed of one connection.
    factory: Mutex<Option<FeedFactory>>,
    config: FeedConfig,
    stats: Arc<FeedStats>,
    /// Moves when `disconnect` forgets every reference, so a release
    /// answered after it puts nothing back.
    epoch: AtomicU64,
    /// The broker session's refusals, its step before a new instrument is
    /// subscribed and what it hears when one is let go of (`presubscribe`).
    refusal: Mutex<Option<FeedRefusal>>,
    presubscribe: Mutex<Option<Presubscribe>>,
    let_go: Mutex<Option<FeedLetGo>>,
    /// The broker's own words for a subscribe past the room, where its web
    /// adapter refuses before the pool does (`set_room_refusal`).
    room_refusal: Mutex<Option<&'static str>>,
}

impl Default for WebSocketManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSocketManager {
    pub fn new() -> Self {
        Self::with_config(FeedConfig::default())
    }

    pub fn with_config(config: FeedConfig) -> Self {
        let (events, _) = broadcast::channel(config.event_capacity.max(1));
        let packets = PacketOut::with_capacity(config.event_capacity);
        let (status, _) = watch::channel(FeedStatus::Disconnected);
        Self {
            events,
            packets,
            status,
            registry: Arc::new(Mutex::new(Registry::with_limits(PoolLimits::single(
                config.max_instruments,
            )))),
            connections: Mutex::new(Vec::new()),
            factory: Mutex::new(None),
            config,
            stats: Arc::new(FeedStats::default()),
            epoch: AtomicU64::new(0),
            refusal: Mutex::new(None),
            presubscribe: Mutex::new(None),
            let_go: Mutex::new(None),
            room_refusal: Mutex::new(None),
        }
    }

    /// Normalised ticks, depth and order updates. Receivers must handle
    /// `RecvError::Lagged` (skip ahead) rather than treat it as fatal.
    pub fn subscribe_ticks(&self) -> broadcast::Receiver<MarketEvent> {
        self.events.subscribe()
    }

    /// Each broker message's market events as one packet, with the
    /// session that sent them, and each session's opening. Receivers must
    /// handle `RecvError::Lagged` as the tick receivers do.
    pub fn subscribe_packets(&self) -> broadcast::Receiver<MarketPacket> {
        self.packets.subscribe()
    }

    /// Test hook: hand `event` to every tick listener, and as a packet of
    /// its own from the latest session of the first connection, as the
    /// feed task does.
    #[cfg(test)]
    pub(crate) fn emit(&self, event: MarketEvent) {
        self.emit_packet(vec![event]);
    }

    /// Test hook: one broker message's `events`, as the feed task sends
    /// them (one by one, and together as one packet).
    #[cfg(test)]
    pub(crate) fn emit_packet(&self, events: Vec<MarketEvent>) {
        self.emit_packet_from(self.test_session(0), events);
    }

    /// Test hook: as `emit_packet`, from session `from`.
    #[cfg(test)]
    pub(crate) fn emit_packet_from(&self, from: SessionId, events: Vec<MarketEvent>) {
        for e in &events {
            let _ = self.events.send(e.clone());
        }
        self.packets.send(from, events);
    }

    /// Test hook: the latest session number, on connection `conn`.
    #[cfg(test)]
    pub(crate) fn test_session(&self, conn: usize) -> SessionId {
        SessionId {
            conn,
            session: self.packets.latest(),
        }
    }

    /// Test hook: how many listen for ticks now.
    #[cfg(test)]
    pub(crate) fn tick_listeners(&self) -> usize {
        self.events.receiver_count()
    }

    /// Current feed status.
    pub fn status(&self) -> FeedStatus {
        self.status.borrow().clone()
    }

    /// Status changes.
    pub fn watch_status(&self) -> watch::Receiver<FeedStatus> {
        self.status.subscribe()
    }

    pub fn stats(&self) -> &FeedStats {
        &self.stats
    }

    pub fn is_connected(&self) -> bool {
        matches!(*self.status.borrow(), FeedStatus::Connected { .. })
    }

    /// Whether the first connection's task is alive (connected or
    /// retrying).
    pub fn is_running(&self) -> bool {
        self.connections
            .lock()
            .first()
            .and_then(Option::as_ref)
            .is_some_and(|r| !r.task.is_finished())
    }

    /// Number of distinct instruments currently subscribed.
    pub fn instrument_count(&self) -> usize {
        self.registry.lock().entries.len()
    }

    /// The effective subscription of every registered instrument.
    pub fn subscriptions(&self) -> Vec<FeedSubscription> {
        self.registry.lock().effective_all()
    }

    /// The room the feed has: instruments per connection, and connections.
    pub fn limits(&self) -> PoolLimits {
        self.registry.lock().limits
    }

    /// Start (or restart) the feed on one connection with this adapter,
    /// carrying at most `FeedConfig::max_instruments`. Existing
    /// subscriptions are kept and sent again on the new socket.
    pub async fn connect(&self, feed: Box<dyn BrokerFeed>) -> Result<()> {
        let limits = PoolLimits::single(self.config.max_instruments);
        self.start(feed, None, limits).await;
        Ok(())
    }

    /// Start (or restart) the feed as a pool of connections (audit C07):
    /// `feed` for the first, `more` for each further one, opened once the
    /// open ones are full; `limits` how many instruments each carries and
    /// how many there may be. Existing subscriptions are kept and sent
    /// again.
    pub async fn connect_pool(
        &self,
        feed: Box<dyn BrokerFeed>,
        more: FeedFactory,
        limits: PoolLimits,
    ) -> Result<()> {
        self.start(feed, Some(more), limits).await;
        Ok(())
    }

    async fn start(
        &self,
        feed: Box<dyn BrokerFeed>,
        more: Option<FeedFactory>,
        limits: PoolLimits,
    ) {
        self.stop_all().await;
        self.stats.begin(feed.broker());
        let extra: Vec<usize> = {
            let mut reg = self.registry.lock();
            reg.set_limits(limits);
            (1..reg.per_conn.len())
                .filter(|c| reg.per_conn[*c] > 0)
                .collect()
        };
        *self.factory.lock() = more;
        let first = self.spawn(0, feed);
        {
            let mut connections = self.connections.lock();
            connections.clear();
            connections.push(Some(first));
        }
        for conn in extra {
            self.open_extra(conn);
        }
    }

    /// Drop the sockets and connect again at once, keeping every
    /// subscription: after the computer slept a socket is likely dead
    /// though no error has come yet (ARCH-03). Nothing when no feed runs,
    /// or for a connection whose command queue is full (it is busy with
    /// commands and will notice a dead socket itself).
    pub fn reconnect(&self) {
        for r in self.connections.lock().iter().flatten() {
            let _ = r.cmd.try_send(Command::Reconnect);
        }
    }

    /// Connect again each connection the broker ended for a reason the
    /// trader can clear (`FeedStatus::Halted`: too many connections on one
    /// login), and only those: a feed client signed in, as the web rebuilds
    /// a disconnected adapter when a client authenticates
    /// (`websocket_proxy/server.py` 905-931), so closing the other app and
    /// reconnecting brings prices back without a logout.
    pub fn resume(&self) {
        for r in self.connections.lock().iter().flatten() {
            if matches!(*r.status.borrow(), FeedStatus::Halted { .. }) {
                let _ = r.cmd.try_send(Command::Reconnect);
            }
        }
    }

    /// Stop the feed and forget every subscription (logout, broker switch).
    pub async fn disconnect(&self) -> Result<()> {
        self.stop_all().await;
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.registry.lock().clear();
        *self.factory.lock() = None;
        *self.refusal.lock() = None;
        *self.presubscribe.lock() = None;
        *self.let_go.lock() = None;
        *self.room_refusal.lock() = None;
        self.status.send_replace(FeedStatus::Disconnected);
        Ok(())
    }

    async fn stop_all(&self) {
        let all: Vec<Running> = self.connections.lock().drain(..).flatten().collect();
        for r in all {
            pool::stop(r).await;
        }
    }

    /// The broker's words for a subscribe past this feed's room, where its
    /// web adapter refuses it before the pool does (Dhan's 20-level socket),
    /// or none for the pool's words (the default; a disconnect clears it).
    pub fn set_room_refusal(&self, words: Option<&'static str>) {
        *self.room_refusal.lock() = words;
    }

    /// What a reference past the feed's room is refused with: the broker's
    /// words when it has its own (`set_room_refusal`), else the web pool's
    /// MAX_CAPACITY_REACHED words (`websocket_proxy/
    /// connection_manager.py:364-369`) with this feed's own limits.
    fn no_room(&self, reg: &Registry) -> String {
        if let Some(words) = *self.room_refusal.lock() {
            return words.to_string();
        }
        let limits = reg.limits;
        format!(
            "Maximum capacity reached: {} connections \u{d7} {} symbols = {} symbols. \
             Currently subscribed to {} symbols.",
            limits.connections,
            limits.per_connection,
            limits.total(),
            reg.per_conn.iter().sum::<usize>()
        )
    }

    /// Add references, all or none; the broker is told only about
    /// instruments whose effective mode changed. A broker that answers
    /// subscribes apart from the socket is waited on, and a refusal fails
    /// the call, letting go of what it took (review R107-03).
    pub async fn subscribe(&self, subs: Vec<FeedSubscription>) -> Result<()> {
        let subs = self
            .presubscribed(subs)
            .await
            .into_iter()
            .collect::<std::result::Result<Vec<_>, String>>()
            .map_err(AppError::Validation)?;
        let usable = self.usable();
        let (placed, deltas) = {
            let mut reg = self.registry.lock();
            let open = |c: usize| usable.get(c).copied().unwrap_or(true);
            let free: usize = (0..reg.limits.connections)
                .filter(|c| open(*c))
                .map(|c| {
                    let held = reg.per_conn.get(c).copied().unwrap_or(0);
                    reg.limits.per_connection.saturating_sub(held)
                })
                .sum();
            if reg.new_instruments(&subs) > free {
                return Err(AppError::Validation(self.no_room(&reg)));
            }
            let mut deltas = Vec::new();
            let mut placed = Vec::new();
            for s in subs {
                if let Some(conn) = reg.place(&s, &open) {
                    placed.push(Some(s.clone()));
                    deltas.extend(reg.add(s, conn));
                }
            }
            (placed, (deltas, reg.changes))
        };
        let not_taken = self.apply_admitted(deltas).await;
        let Some(why) = not_taken.refusals.values().next().cloned() else {
            return Ok(());
        };
        // All or none: what the broker took goes again with the refusal.
        self.let_go_refused(&placed, &not_taken).await;
        let taken: Vec<FeedSubscription> = placed
            .into_iter()
            .flatten()
            .filter(|s| !not_taken.refusals.contains_key(&s.instrument_key()))
            .collect();
        let _ = self.unsubscribe(taken).await;
        Err(AppError::Validation(why))
    }

    /// Add references one by one, each while a connection has room for
    /// another instrument, counted across every consumer of this manager
    /// (feed clients, the sandbox, strategies, Flow): one answer each, so
    /// a batch that crosses the limit still starts its admissible members
    /// and refuses the rest with why (audit C02). What the broker refuses
    /// (`set_refusal`) is refused, and a new instrument goes through the
    /// broker's step first (`set_presubscribe`), in their words.
    pub async fn admit(&self, subs: Vec<FeedSubscription>) -> Vec<std::result::Result<(), String>> {
        let prepared = self.presubscribed(subs).await;
        let usable = self.usable();
        let (mut answers, placed, deltas) = {
            let mut reg = self.registry.lock();
            let open = |c: usize| usable.get(c).copied().unwrap_or(true);
            let mut answers = Vec::with_capacity(prepared.len());
            let mut placed = Vec::with_capacity(prepared.len());
            let mut deltas = Vec::new();
            for p in prepared {
                let s = match p {
                    Ok(s) => s,
                    Err(why) => {
                        answers.push(Err(why));
                        placed.push(None);
                        continue;
                    }
                };
                match reg.place(&s, &open) {
                    Some(conn) => {
                        placed.push(Some(s.clone()));
                        deltas.extend(reg.add(s, conn));
                        answers.push(Ok(()));
                    }
                    None => {
                        placed.push(None);
                        answers.push(Err(self.no_room(&reg)));
                    }
                }
            }
            (answers, placed, (deltas, reg.changes))
        };
        // Taken once the broker took it (review R107-03).
        let not_taken = self.apply_admitted(deltas).await;
        if !not_taken.refusals.is_empty() {
            self.let_go_refused(&placed, &not_taken).await;
            for (answer, s) in answers.iter_mut().zip(&placed) {
                let why = s
                    .as_ref()
                    .and_then(|s| not_taken.refusals.get(&s.instrument_key()));
                if let Some(why) = why {
                    *answer = Err(why.clone());
                }
            }
        }
        answers
    }

    /// Drop references; an instrument with none left is unsubscribed.
    pub async fn unsubscribe(&self, subs: Vec<FeedSubscription>) -> Result<()> {
        let deltas = {
            let mut reg = self.registry.lock();
            let removed = subs.iter().filter_map(|s| reg.remove(s)).collect();
            (removed, reg.changes)
        };
        self.apply(deltas).await;
        self.tell_let_go(&subs);
        Ok(())
    }

    /// Drop every subscription (the feed stays connected).
    pub async fn unsubscribe_all(&self) -> Result<()> {
        let (deltas, gone) = {
            let mut reg = self.registry.lock();
            let gone = reg.effective_all();
            let all = reg
                .entries
                .values()
                .filter_map(|e| Some((e.conn, Delta::Unsubscribe(e.effective()?))))
                .collect();
            reg.clear();
            ((all, reg.changes), gone)
        };
        self.apply(deltas).await;
        self.tell_let_go(&gone);
        Ok(())
    }

    /// Hand each connection its changes, made when the registry's change
    /// count was `seq`. An extra connection that is not open yet is opened
    /// (its first subscription sends what it carries); one that carries
    /// nothing any more is closed.
    async fn apply(&self, (deltas, seq): (Vec<Routed>, u64)) {
        if deltas.is_empty() {
            return;
        }
        for (conn, deltas) in by_conn(deltas) {
            let tx = self
                .connections
                .lock()
                .get(conn)
                .and_then(Option::as_ref)
                .map(|r| r.cmd.clone());
            match tx {
                // Bounded: back-pressure rather than unbounded growth. If
                // the task is gone the registry still holds the state for
                // the next connection.
                Some(tx) => {
                    let _ = tx.send(Command::Apply(Apply::plain(deltas, seq))).await;
                }
                None if conn > 0 => self.open_extra(conn),
                None => {}
            }
        }
        self.close_idle().await;
    }
}

impl Drop for WebSocketManager {
    fn drop(&mut self) {
        for r in self.connections.get_mut().drain(..).flatten() {
            r.task.abort();
        }
    }
}

#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod deadline_tests;
#[cfg(test)]
mod packet_tests;
#[cfg(test)]
mod pool_tests;
#[cfg(test)]
mod presubscribe_tests;
#[cfg(test)]
mod release_tests;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod tests;
