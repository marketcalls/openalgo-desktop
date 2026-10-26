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
//! is bounded by what clients currently hold.
//!
//! Resource ownership: the only spawned task is the supervisor, kept as a
//! `JoinHandle` and aborted (after a graceful stop request) on `disconnect`.
//! The command channel is bounded; the event channel is a bounded broadcast
//! whose slow receivers see `Lagged` and skip ahead.

use crate::brokers::common::ratelimit::backoff_delay;
use crate::brokers::common::redact::{url_safe_error, ws_error_kind};
use crate::brokers::common::streaming::{
    normalize_request, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, MarketEvent, Message,
    PrepareError,
};
use crate::error::{AppError, Result};
use futures_util::{SinkExt, StreamExt};
use heartbeat::{Beat, Heartbeat};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Error as WsError;

mod heartbeat;
mod losses;
mod stats;
pub use losses::{Reader, SinceConnect};
pub use stats::{Connection, DropCause, FeedStats, LastDrop};

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
    /// Most instruments one connection may carry (Kite: 3000).
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
}

/// Change to apply on the live socket.
#[derive(Debug, Clone)]
enum Delta {
    Subscribe(FeedSubscription),
    Unsubscribe(FeedSubscription),
    ModeChange(FeedSubscription, FeedSubscription),
}

enum Command {
    Apply(Vec<Delta>),
    /// Drop the socket and connect again (after the computer slept).
    Reconnect,
    Stop,
}

struct Entry {
    base: FeedSubscription,
    modes: BTreeMap<FeedMode, u32>,
}

impl Entry {
    fn effective(&self) -> Option<FeedSubscription> {
        self.modes
            .keys()
            .next_back()
            .map(|m| self.base.with_mode(*m))
    }
}

#[derive(Default)]
struct Registry {
    entries: HashMap<(String, String), Entry>,
}

impl Registry {
    fn effective_all(&self) -> Vec<FeedSubscription> {
        self.entries.values().filter_map(Entry::effective).collect()
    }
}

struct Running {
    cmd: mpsc::Sender<Command>,
    task: JoinHandle<()>,
}

pub struct WebSocketManager {
    events: broadcast::Sender<MarketEvent>,
    status: watch::Sender<FeedStatus>,
    registry: Arc<Mutex<Registry>>,
    running: Mutex<Option<Running>>,
    config: FeedConfig,
    stats: Arc<FeedStats>,
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
        let (status, _) = watch::channel(FeedStatus::Disconnected);
        Self {
            events,
            status,
            registry: Arc::new(Mutex::new(Registry::default())),
            running: Mutex::new(None),
            config,
            stats: Arc::new(FeedStats::default()),
        }
    }

    /// Normalised ticks, depth and order updates. Receivers must handle
    /// `RecvError::Lagged` (skip ahead) rather than treat it as fatal.
    pub fn subscribe_ticks(&self) -> broadcast::Receiver<MarketEvent> {
        self.events.subscribe()
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

    /// Whether a supervisor task is alive (connected or retrying).
    pub fn is_running(&self) -> bool {
        self.running
            .lock()
            .as_ref()
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

    /// Start (or restart) the feed with this broker adapter. Existing
    /// subscriptions are kept and sent again on the new socket.
    pub async fn connect(&self, feed: Box<dyn BrokerFeed>) -> Result<()> {
        self.stop_task().await;
        self.stats.begin(feed.broker());
        let (tx, rx) = mpsc::channel(self.config.command_capacity.max(1));
        let ctx = Supervisor {
            feed,
            cmd: rx,
            registry: self.registry.clone(),
            events: self.events.clone(),
            status: self.status.clone(),
            config: self.config.clone(),
            stats: self.stats.clone(),
        };
        let task = tokio::spawn(ctx.run());
        *self.running.lock() = Some(Running { cmd: tx, task });
        Ok(())
    }

    /// Drop the socket and connect again at once, keeping every
    /// subscription: after the computer slept the socket is likely dead
    /// though no error has come yet (ARCH-03). Nothing when no feed runs,
    /// or when the command queue is full (the session is busy with commands
    /// and will notice a dead socket itself).
    pub fn reconnect(&self) {
        if let Some(r) = self.running.lock().as_ref() {
            let _ = r.cmd.try_send(Command::Reconnect);
        }
    }

    /// Stop the feed and forget every subscription (logout, broker switch).
    pub async fn disconnect(&self) -> Result<()> {
        self.stop_task().await;
        self.registry.lock().entries.clear();
        self.status.send_replace(FeedStatus::Disconnected);
        Ok(())
    }

    async fn stop_task(&self) {
        let running = self.running.lock().take();
        if let Some(r) = running {
            // Ask for a clean close; abort if the task does not finish soon.
            let _ = r.cmd.try_send(Command::Stop);
            let mut task = r.task;
            if tokio::time::timeout(Duration::from_secs(2), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }

    /// Add references; the broker is told only about instruments whose
    /// effective mode changed.
    pub async fn subscribe(&self, subs: Vec<FeedSubscription>) -> Result<()> {
        let deltas = {
            let mut reg = self.registry.lock();
            let new_instruments = subs
                .iter()
                .filter(|s| !reg.entries.contains_key(&s.instrument_key()))
                .map(|s| s.instrument_key())
                .collect::<std::collections::HashSet<_>>()
                .len();
            if reg.entries.len() + new_instruments > self.config.max_instruments {
                return Err(AppError::Validation(format!(
                    "You can stream at most {} instruments at once. Unsubscribe some before adding more.",
                    self.config.max_instruments
                )));
            }
            let mut deltas = Vec::new();
            for s in subs {
                let entry = reg
                    .entries
                    .entry(s.instrument_key())
                    .or_insert_with(|| Entry {
                        base: s.clone(),
                        modes: BTreeMap::new(),
                    });
                let before = entry.effective();
                *entry.modes.entry(s.mode).or_insert(0) += 1;
                let after = entry.effective();
                match (before, after) {
                    (None, Some(a)) => deltas.push(Delta::Subscribe(a)),
                    (Some(b), Some(a)) if b.mode != a.mode => deltas.push(Delta::ModeChange(b, a)),
                    _ => {}
                }
            }
            deltas
        };
        self.apply(deltas).await;
        Ok(())
    }

    /// Drop references; an instrument with none left is unsubscribed.
    pub async fn unsubscribe(&self, subs: Vec<FeedSubscription>) -> Result<()> {
        let deltas = {
            let mut reg = self.registry.lock();
            let mut deltas = Vec::new();
            for s in subs {
                let key = s.instrument_key();
                let Some(entry) = reg.entries.get_mut(&key) else {
                    continue;
                };
                let before = entry.effective();
                if let Some(n) = entry.modes.get_mut(&s.mode) {
                    *n -= 1;
                    if *n == 0 {
                        entry.modes.remove(&s.mode);
                    }
                }
                let after = entry.effective();
                match (before, after) {
                    (Some(b), None) => {
                        reg.entries.remove(&key);
                        deltas.push(Delta::Unsubscribe(b));
                    }
                    (Some(b), Some(a)) if b.mode != a.mode => deltas.push(Delta::ModeChange(b, a)),
                    _ => {}
                }
            }
            deltas
        };
        self.apply(deltas).await;
        Ok(())
    }

    /// Drop every subscription (the feed stays connected).
    pub async fn unsubscribe_all(&self) -> Result<()> {
        let deltas: Vec<Delta> = {
            let mut reg = self.registry.lock();
            let all = reg.effective_all();
            reg.entries.clear();
            all.into_iter().map(Delta::Unsubscribe).collect()
        };
        self.apply(deltas).await;
        Ok(())
    }

    async fn apply(&self, deltas: Vec<Delta>) {
        if deltas.is_empty() {
            return;
        }
        let tx = self.running.lock().as_ref().map(|r| r.cmd.clone());
        if let Some(tx) = tx {
            // Bounded: back-pressure rather than unbounded growth. If the
            // task is gone the registry still holds the state for the next
            // connection.
            let _ = tx.send(Command::Apply(deltas)).await;
        }
    }
}

impl Drop for WebSocketManager {
    fn drop(&mut self) {
        if let Some(r) = self.running.get_mut().take() {
            r.task.abort();
        }
    }
}

/// Why one socket session ended.
enum SessionEnd {
    Stopped,
    /// The connection ended (or was not made) for this cause; the manager
    /// connects again after a backoff.
    Dropped(DropCause),
    AuthFailed(String),
}

/// A session goes on, or ends with the reason (`?` in its steps).
type Sent = std::result::Result<(), SessionEnd>;

const CONNECT_FAILED: SessionEnd = SessionEnd::Dropped(DropCause::ConnectFailed);

/// What a login acknowledgement that did not come in time means.
#[derive(Debug, Clone, Copy)]
enum AckLapse {
    /// It is optional: take the session as accepted.
    Assume,
    /// It is required: the session is lost.
    Refuse,
}

struct Supervisor {
    feed: Box<dyn BrokerFeed>,
    cmd: mpsc::Receiver<Command>,
    registry: Arc<Mutex<Registry>>,
    events: broadcast::Sender<MarketEvent>,
    status: watch::Sender<FeedStatus>,
    config: FeedConfig,
    stats: Arc<FeedStats>,
}

impl Supervisor {
    async fn run(mut self) {
        let broker = self.feed.broker().to_string();
        let mut attempt: u32 = 0;
        loop {
            self.status.send_replace(FeedStatus::Connecting {
                broker: broker.clone(),
                attempt,
            });
            let started = Instant::now();
            let end = self.connect_once(&broker).await;
            match end {
                SessionEnd::Stopped => {
                    self.status.send_replace(FeedStatus::Disconnected);
                    return;
                }
                SessionEnd::AuthFailed(message) => {
                    tracing::warn!(broker = %broker, "Market data feed refused the session");
                    self.status.send_replace(FeedStatus::AuthFailed {
                        broker: broker.clone(),
                        message,
                    });
                    // Do not hammer the broker with a dead token; wait for
                    // a stop (logout) or a new `connect`.
                    while let Some(cmd) = self.cmd.recv().await {
                        if matches!(cmd, Command::Stop) {
                            break;
                        }
                    }
                    return;
                }
                SessionEnd::Dropped(cause) => self.stats.dropped(cause),
            }
            if started.elapsed() >= self.config.stable_after {
                attempt = 0;
            }
            let delay = backoff_delay(attempt, self.config.backoff_base, self.config.backoff_max);
            attempt = attempt.saturating_add(1);
            self.status.send_replace(FeedStatus::Reconnecting {
                broker: broker.clone(),
                attempt,
                delay_ms: delay.as_millis() as u64,
            });
            tracing::info!(broker = %broker, attempt, "Market data feed reconnecting in {:?}", delay);
            // Sleep, still answering commands (registry already holds the
            // state; a stop ends the loop).
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    cmd = self.cmd.recv() => match cmd {
                        None | Some(Command::Stop) => {
                            self.status.send_replace(FeedStatus::Disconnected);
                            return;
                        }
                        // Connect now rather than at the end of the backoff.
                        Some(Command::Reconnect) => break,
                        Some(Command::Apply(_)) => {}
                    }
                }
            }
        }
    }

    async fn connect_once(&mut self, broker: &str) -> SessionEnd {
        match tokio::time::timeout(self.config.connect_timeout, self.feed.prepare()).await {
            Ok(Ok(())) => {}
            Ok(Err(PrepareError::AuthFailed(message))) => return SessionEnd::AuthFailed(message),
            Ok(Err(PrepareError::Unavailable)) => return CONNECT_FAILED,
            Err(_) => {
                tracing::debug!(broker, "Market data feed preparation timed out");
                return CONNECT_FAILED;
            }
        }
        let mut retried = false;
        let ws = loop {
            let request = match self.feed.ws_request() {
                Ok(r) => normalize_request(r),
                Err(e) => {
                    tracing::error!(
                        broker,
                        code = e.code(),
                        "Market data feed request could not be built"
                    );
                    return SessionEnd::AuthFailed(e.client_message());
                }
            };
            let connect = tokio_tungstenite::connect_async(request);
            match tokio::time::timeout(self.config.connect_timeout, connect).await {
                Ok(Err(e)) if !matches!(e, WsError::Http(_)) && !retried => {
                    if self.feed.on_connect_failed(&url_safe_error(&e)) {
                        retried = true;
                        continue;
                    }
                    break Ok(Err(e));
                }
                other => break other,
            }
        };
        let ws = match ws {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(WsError::Http(resp))) => {
                let code = resp.status().as_u16();
                tracing::warn!(broker, status = code, "Market data feed handshake refused");
                if self.feed.is_auth_failure(Some(code)) {
                    return SessionEnd::AuthFailed(
                        "The broker refused the live market data session. Log in to your broker again."
                            .into(),
                    );
                }
                return CONNECT_FAILED;
            }
            Ok(Err(e)) => {
                tracing::debug!(
                    broker,
                    kind = ws_error_kind(&e),
                    "Market data feed connect failed"
                );
                return CONNECT_FAILED;
            }
            Err(_) => {
                tracing::debug!(broker, "Market data feed connect timed out");
                return CONNECT_FAILED;
            }
        };
        self.stats.opened();
        let (mut write, mut read) = ws.split();
        let end = self.session(broker, &mut write, &mut read).await;
        // Close before reconnecting so the descriptor is released on every
        // path; bounded so a dead peer cannot hold us.
        let _ = tokio::time::timeout(Duration::from_secs(2), write.close()).await;
        drop(write);
        drop(read);
        end
    }

    /// Send `frames` in order, all within the write timeout. An error when
    /// the socket failed or did not take them in time (TIME-02): the
    /// session then ends and the socket is replaced.
    async fn send_all<S>(&self, write: &mut S, frames: Vec<Message>) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        if frames.is_empty() {
            return Ok(());
        }
        let send = async {
            for f in frames {
                write.send(f).await?;
            }
            Ok::<(), WsError>(())
        };
        match tokio::time::timeout(self.config.write_timeout, send).await {
            Ok(sent) => sent.map_err(|_| SessionEnd::Dropped(DropCause::Closed)),
            Err(_) => {
                tracing::warn!(
                    broker = self.feed.broker(),
                    "Market data feed stopped taking frames; reconnecting"
                );
                Err(SessionEnd::Dropped(DropCause::NotTakingFrames))
            }
        }
    }

    /// Sends the full subscription set and reports the feed connected.
    async fn subscribe_all<S>(&mut self, broker: &str, write: &mut S) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        self.resubscribe(write).await?;
        self.stats.mark_connected();
        self.status.send_replace(FeedStatus::Connected {
            broker: broker.to_string(),
        });
        Ok(())
    }

    /// One message from the broker: market events published, replies sent,
    /// and the subscriptions sent once the login is acknowledged. An `Err`
    /// ends the session.
    async fn on_message<S>(
        &mut self,
        broker: &str,
        write: &mut S,
        msg: Message,
        subscribed: &mut bool,
    ) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        if let Message::Close(_) = msg {
            return Err(SessionEnd::Dropped(DropCause::Closed));
        }
        for ev in self.feed.parse(&msg) {
            match ev {
                FeedEvent::AuthFailed(m) => return Err(SessionEnd::AuthFailed(m)),
                FeedEvent::AuthOk => {
                    if !*subscribed {
                        self.subscribe_all(broker, write).await?;
                        *subscribed = true;
                    }
                }
                FeedEvent::Heartbeat => {}
                FeedEvent::Reply(m) => self.send_all(write, vec![m]).await?,
                e @ (FeedEvent::Tick(_) | FeedEvent::Depth(_) | FeedEvent::OrderUpdate(_)) => {
                    self.stats.mark_event();
                    // No receiver yet is normal; nothing is retained.
                    let _ = self.events.send(Arc::new(e));
                }
            }
        }
        Ok(())
    }

    /// Coalesces the commands already waiting into `deltas`, so a burst of
    /// subscribe calls reaches the broker as batches. The end of the session
    /// when a stop or a reconnect was among them.
    fn coalesce(&mut self, deltas: &mut Vec<Delta>) -> Option<SessionEnd> {
        while let Ok(more) = self.cmd.try_recv() {
            match more {
                Command::Apply(d) => deltas.extend(d),
                Command::Reconnect => return Some(self.reconnecting()),
                Command::Stop => return Some(SessionEnd::Stopped),
            }
        }
        None
    }

    /// A reconnect was asked for: the session ends, and the registry is
    /// sent again on the new socket.
    fn reconnecting(&self) -> SessionEnd {
        tracing::info!(
            broker = self.feed.broker(),
            "Market data feed reconnecting after the computer slept"
        );
        SessionEnd::Dropped(DropCause::Woke)
    }

    /// Whether the feed stalled: nothing received within the stall timeout,
    /// or no market data while the socket stayed open.
    fn stalled(&mut self, broker: &str, last_rx: Instant) -> Option<DropCause> {
        if last_rx.elapsed() >= self.config.stall_timeout {
            tracing::warn!(broker, "Market data feed stalled; reconnecting");
            return Some(DropCause::Silent);
        }
        if self.feed.data_stalled() {
            tracing::warn!(
                broker,
                "Market data stopped while the socket stayed open; reconnecting"
            );
            return Some(DropCause::DataStopped);
        }
        None
    }

    async fn session<S, R>(&mut self, broker: &str, write: &mut S, read: &mut R) -> SessionEnd
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
        R: futures_util::Stream<Item = std::result::Result<Message, WsError>> + Unpin,
    {
        let hello = self.feed.on_connected();
        if let Err(end) = self.send_all(write, hello).await {
            return end;
        }
        let mut subscribed = false;
        if !self.feed.awaits_auth_ack() {
            if let Err(end) = self.subscribe_all(broker, write).await {
                return end;
            }
            subscribed = true;
        }
        let mut heartbeat = Heartbeat::new(&*self.feed);
        let check = (self.config.stall_timeout / 4).max(Duration::from_millis(10));
        let mut watchdog = tokio::time::interval_at(Instant::now() + check, check);
        let mut last_rx = Instant::now();
        let ack = self.ack_wait(subscribed);
        let ack_deadline =
            tokio::time::sleep(ack.map_or(Duration::from_secs(3600), |(after, _)| after));
        tokio::pin!(ack_deadline);
        loop {
            tokio::select! {
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            tracing::debug!(broker, kind = ws_error_kind(&e), "Market data feed read error");
                            return SessionEnd::Dropped(DropCause::Closed);
                        }
                        None => return SessionEnd::Dropped(DropCause::Closed),
                    };
                    last_rx = Instant::now();
                    heartbeat.heard();
                    if let Err(end) = self.on_message(broker, write, msg, &mut subscribed).await {
                        return end;
                    }
                }
                cmd = self.cmd.recv() => {
                    if let Err(end) = self.on_command(write, cmd, subscribed).await {
                        return end;
                    }
                }
                beat = heartbeat.next() => match beat {
                    Beat::Due(m) => {
                        if let Err(end) = self.send_all(write, vec![m]).await {
                            return end;
                        }
                        heartbeat.sent();
                    }
                    Beat::Unanswered => {
                        tracing::warn!(broker, "The broker did not answer the feed's heartbeat; reconnecting");
                        return SessionEnd::Dropped(DropCause::HeartbeatUnanswered);
                    }
                },
                _ = &mut ack_deadline, if ack.is_some() && !subscribed => {
                    let lapse = ack.map_or(AckLapse::Refuse, |(_, lapse)| lapse);
                    if let Err(end) = self.on_ack_lapse(broker, write, lapse).await {
                        return end;
                    }
                    subscribed = true;
                }
                _ = watchdog.tick() => {
                    if let Some(cause) = self.stalled(broker, last_rx) {
                        return SessionEnd::Dropped(cause);
                    }
                }
            }
        }
    }

    /// The login acknowledgement this session still waits for, and what
    /// its absence means once the wait is over; `None` when nothing is
    /// awaited.
    fn ack_wait(&self, subscribed: bool) -> Option<(Duration, AckLapse)> {
        if subscribed {
            return None;
        }
        // An optional acknowledgement (Groww: 2 s, like the web).
        if let Some(after) = self.feed.auth_ack_timeout() {
            return Some((after, AckLapse::Assume));
        }
        self.feed
            .auth_ack_deadline()
            .map(|after| (after, AckLapse::Refuse))
    }

    /// The broker did not acknowledge the login in time: take the session
    /// as accepted when the acknowledgement is optional, else end it so the
    /// capped backoff reconnects (TIME-05). Frames that keep arriving
    /// without an acknowledgement (heartbeats) must not keep a socket that
    /// never streams.
    async fn on_ack_lapse<S>(&mut self, broker: &str, write: &mut S, lapse: AckLapse) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        match lapse {
            AckLapse::Assume => self.subscribe_all(broker, write).await,
            AckLapse::Refuse => {
                tracing::warn!(
                    broker,
                    "The broker did not confirm the market data login in time; reconnecting"
                );
                Err(SessionEnd::Dropped(DropCause::LoginNotConfirmed))
            }
        }
    }

    /// One command from the manager; subscription changes wait for the
    /// login acknowledgement (they go out with the full set after it).
    async fn on_command<S>(&mut self, write: &mut S, cmd: Option<Command>, subscribed: bool) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        let mut deltas = match cmd {
            Some(Command::Apply(d)) => d,
            Some(Command::Reconnect) => return Err(self.reconnecting()),
            None | Some(Command::Stop) => return Err(SessionEnd::Stopped),
        };
        if let Some(end) = self.coalesce(&mut deltas) {
            return Err(end);
        }
        if !subscribed {
            return Ok(());
        }
        let frames = self.delta_frames(deltas);
        self.send_all(write, frames).await
    }

    /// Frames for a run of deltas, in order. Consecutive subscribes (and
    /// consecutive unsubscribes) go to the feed in one call, so feeds that
    /// can batch send one frame or request per run rather than one per
    /// instrument (web #2176: Firstock, Pocketful, RMoney batching).
    fn delta_frames(&mut self, deltas: Vec<Delta>) -> Vec<Message> {
        let mut frames = Vec::new();
        let mut subs: Vec<FeedSubscription> = Vec::new();
        let mut unsubs: Vec<FeedSubscription> = Vec::new();
        for d in deltas {
            match d {
                Delta::Subscribe(s) => {
                    if !unsubs.is_empty() {
                        frames.extend(self.feed.unsubscribe_frames(&std::mem::take(&mut unsubs)));
                    }
                    subs.push(s);
                }
                Delta::Unsubscribe(s) => {
                    if !subs.is_empty() {
                        frames.extend(self.feed.subscribe_frames(&std::mem::take(&mut subs)));
                    }
                    unsubs.push(s);
                }
                Delta::ModeChange(old, new) => {
                    if !subs.is_empty() {
                        frames.extend(self.feed.subscribe_frames(&std::mem::take(&mut subs)));
                    }
                    if !unsubs.is_empty() {
                        frames.extend(self.feed.unsubscribe_frames(&std::mem::take(&mut unsubs)));
                    }
                    frames.extend(self.feed.mode_change_frames(&old, &new));
                }
            }
        }
        if !subs.is_empty() {
            frames.extend(self.feed.subscribe_frames(&subs));
        }
        if !unsubs.is_empty() {
            frames.extend(self.feed.unsubscribe_frames(&unsubs));
        }
        frames
    }

    async fn resubscribe<S>(&mut self, write: &mut S) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        // The session was just accepted: its post-login frames go first.
        let mut frames = self.feed.on_authenticated();
        let subs = self.registry.lock().effective_all();
        if !subs.is_empty() {
            frames.extend(self.feed.subscribe_frames(&subs));
        }
        self.send_all(write, frames).await
    }
}

#[cfg(test)]
mod deadline_tests;
#[cfg(test)]
mod tests;
