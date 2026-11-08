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

use crate::brokers::common::streaming::{BrokerFeed, FeedSubscription, MarketEvent};
use crate::error::{AppError, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

mod heartbeat;
mod losses;
mod registry;
mod release;
mod stats;
mod supervisor;
pub use losses::{Reader, SinceConnect};
use registry::{Delta, Registry};
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

/// A change for the socket.
struct Apply {
    deltas: Vec<Delta>,
    /// A caller waits to hear what the socket did with it (a release,
    /// audit C01); dropped unanswered when no socket carried it.
    reply: Option<oneshot::Sender<Written>>,
    /// The references the change dropped, put back in the registry when
    /// the socket does not take it, so the next connection sends them.
    undo: Vec<FeedSubscription>,
}

impl Apply {
    fn plain(deltas: Vec<Delta>) -> Self {
        Self {
            deltas,
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
    /// Moves when `disconnect` forgets every reference, so a release
    /// answered after it puts nothing back.
    epoch: AtomicU64,
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
            epoch: AtomicU64::new(0),
        }
    }

    /// Normalised ticks, depth and order updates. Receivers must handle
    /// `RecvError::Lagged` (skip ahead) rather than treat it as fatal.
    pub fn subscribe_ticks(&self) -> broadcast::Receiver<MarketEvent> {
        self.events.subscribe()
    }

    /// Test hook: hand `event` to every tick listener, as the feed task
    /// does.
    #[cfg(test)]
    pub(crate) fn emit(&self, event: MarketEvent) {
        let _ = self.events.send(event);
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
        self.epoch.fetch_add(1, Ordering::SeqCst);
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
            if reg.entries.len() + reg.new_instruments(&subs) > self.config.max_instruments {
                return Err(AppError::Validation(format!(
                    "You can stream at most {} instruments at once. Unsubscribe some before adding more.",
                    self.config.max_instruments
                )));
            }
            subs.into_iter().filter_map(|s| reg.add(s)).collect()
        };
        self.apply(deltas).await;
        Ok(())
    }

    /// Drop references; an instrument with none left is unsubscribed.
    pub async fn unsubscribe(&self, subs: Vec<FeedSubscription>) -> Result<()> {
        let deltas = {
            let mut reg = self.registry.lock();
            subs.iter().filter_map(|s| reg.remove(s)).collect()
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
            let _ = tx.send(Command::Apply(Apply::plain(deltas))).await;
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

#[cfg(test)]
mod deadline_tests;
#[cfg(test)]
mod release_tests;
#[cfg(test)]
mod tests;
