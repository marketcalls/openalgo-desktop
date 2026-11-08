//! The broker connections one manager opens for its market data, as the
//! web's `websocket_proxy/connection_manager.ConnectionPool` does (audit
//! C07): instruments go on the first connection with room, a new
//! connection opens once every open one is full, up to the broker's limit,
//! and an extra connection that carries nothing any more closes. Each has
//! its own socket task (`Supervisor`); all publish on the manager's one
//! event channel, and only the first relays the broker's order updates,
//! so a broker that sends them on every socket (Kite) is heard once.

use super::{Command, FeedStatus, Running, Supervisor, WebSocketManager};
use crate::brokers::common::streaming::BrokerFeed;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// How many instruments one broker connection carries and how many
/// connections the feed may open for them (web `ConnectionPool`:
/// `MAX_SYMBOLS_PER_WEBSOCKET` 1,000 and `MAX_WEBSOCKET_CONNECTIONS` 3,
/// unless the broker's own limits are lower).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolLimits {
    pub per_connection: usize,
    pub connections: usize,
}

impl PoolLimits {
    /// The web's defaults (`websocket_proxy/base_adapter.py`).
    pub const WEB_DEFAULT: PoolLimits = PoolLimits {
        per_connection: 1000,
        connections: 3,
    };

    /// One connection carrying at most `per_connection` instruments.
    pub const fn single(per_connection: usize) -> Self {
        Self {
            per_connection,
            connections: 1,
        }
    }

    /// Instruments the pool can carry in all.
    pub fn total(&self) -> usize {
        self.per_connection.saturating_mul(self.connections)
    }
}

/// Makes the feed adapter for another connection of the same session.
pub type FeedFactory = Arc<dyn Fn() -> Option<Box<dyn BrokerFeed>> + Send + Sync>;

impl WebSocketManager {
    /// Start connection `conn` on `feed`. The first reports the manager's
    /// status; the others their own (`connection_states`).
    pub(super) fn spawn(&self, conn: usize, feed: Box<dyn BrokerFeed>) -> Running {
        let (tx, rx) = mpsc::channel(self.config.command_capacity.max(1));
        let (status, seen) = if conn == 0 {
            (self.status.clone(), self.status.subscribe())
        } else {
            let (status, seen) = watch::channel(FeedStatus::Disconnected);
            (status, seen)
        };
        let task = tokio::spawn(
            Supervisor {
                feed,
                cmd: rx,
                registry: self.registry.clone(),
                events: self.events.clone(),
                status,
                config: self.config.clone(),
                stats: self.stats.clone(),
                conn,
            }
            .run(),
        );
        Running {
            cmd: tx,
            task,
            status: seen,
        }
    }

    /// Open connection `conn` for instruments placed on it, from the
    /// session's feed factory; its first subscription sends them. Nothing
    /// without a factory (a single-connection feed) or a feed.
    pub(super) fn open_extra(&self, conn: usize) {
        let Some(factory) = self.factory.lock().clone() else {
            return;
        };
        let mut connections = self.connections.lock();
        if connections.get(conn).is_some_and(Option::is_some) {
            return;
        }
        let Some(feed) = factory() else {
            tracing::warn!("Another market data connection could not be opened");
            return;
        };
        if connections.len() <= conn {
            connections.resize_with(conn + 1, || None);
        }
        tracing::info!(conn, "Opening another market data connection");
        connections[conn] = Some(self.spawn(conn, feed));
    }

    /// Close the extra connections that carry nothing any more.
    pub(super) async fn close_idle(&self) {
        let idle: Vec<Running> = {
            let reg = self.registry.lock();
            let mut connections = self.connections.lock();
            connections
                .iter_mut()
                .enumerate()
                .skip(1)
                .filter(|(c, r)| r.is_some() && reg.per_conn.get(*c).copied().unwrap_or(0) == 0)
                .filter_map(|(_, r)| r.take())
                .collect()
        };
        for r in idle {
            stop(r).await;
        }
    }

    /// Which connections may take another instrument: those the broker has
    /// not refused or ended.
    pub(super) fn usable(&self) -> Vec<bool> {
        let n = self.registry.lock().limits.connections;
        let connections = self.connections.lock();
        (0..n)
            .map(|c| {
                !connections
                    .get(c)
                    .and_then(Option::as_ref)
                    .is_some_and(|r| r.status.borrow().refusal().is_some())
            })
            .collect()
    }

    /// Each connection's instruments and state, first one first.
    pub fn connection_states(&self) -> Vec<(usize, FeedStatus)> {
        let per_conn = self.registry.lock().per_conn.clone();
        let connections = self.connections.lock();
        connections
            .iter()
            .enumerate()
            .filter_map(|(c, r)| {
                let r = r.as_ref()?;
                Some((
                    per_conn.get(c).copied().unwrap_or(0),
                    r.status.borrow().clone(),
                ))
            })
            .collect()
    }
}

/// Ask a connection's task to close, and abort it if it does not soon.
pub(super) async fn stop(r: Running) {
    let _ = r.cmd.try_send(Command::Stop);
    let mut task = r.task;
    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut task)
        .await
        .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}
