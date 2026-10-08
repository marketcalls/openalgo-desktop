//! Order updates by polling the order book (web
//! `websocket_proxy/order_adapter.py` `PollingOrderUpdateAdapter`; Groww
//! has no order socket).
//!
//! One owned task polls every `interval` (clamped to 1..=60 s, web default
//! 5 s). The first poll seeds the snapshot silently; later polls publish an
//! `OrderUpdate` for every order whose `(status, filled quantity)` changed
//! or that is new. The snapshot is rebuilt from each book, so it is bounded
//! by the current order book. Updates go into a bounded channel; when the
//! receiver is dropped the task ends. `OrderPoller::stop` and `Drop` abort
//! the task.

use super::{orders, GrowwCore};
use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub use crate::brokers::common::order_poll::{
    clamp_interval, diff, to_update, Snapshot, CHANNEL_CAPACITY, DEFAULT_INTERVAL, MAX_INTERVAL,
    MIN_INTERVAL,
};

pub struct OrderPoller {
    task: JoinHandle<()>,
}

impl OrderPoller {
    pub(crate) fn start(
        core: GrowwCore,
        auth: AuthToken,
        interval: Duration,
    ) -> Result<(Self, mpsc::Receiver<OrderUpdate>)> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| AppError::Internal("Order updates need the async runtime".into()))?;
        let interval = clamp_interval(interval);
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let task = handle.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut snapshot: Option<Snapshot> = None;
            loop {
                ticker.tick().await;
                if tx.is_closed() {
                    break;
                }
                let book = match orders::get_order_book(&core, &auth).await {
                    Ok(b) => b,
                    Err(AppError::Auth(_)) => {
                        tracing::warn!("Groww order updates stopped: the session has expired");
                        break;
                    }
                    Err(e) => {
                        tracing::debug!("Groww order poll failed: {}", e.code());
                        continue;
                    }
                };
                let (next, changed) = diff(snapshot.as_ref(), &book);
                snapshot = Some(next);
                for u in changed {
                    if tx.send(u).await.is_err() {
                        return;
                    }
                }
            }
        });
        Ok((Self { task }, rx))
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    pub fn stop(self) {
        // Drop aborts.
    }
}

impl Drop for OrderPoller {
    fn drop(&mut self) {
        self.task.abort();
    }
}
