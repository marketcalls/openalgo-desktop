//! Order updates by polling the order book (web
//! `websocket_proxy/order_adapter.py` `PollingOrderUpdateAdapter`; the web
//! lists fivepaisa in `_POLLING_BROKERS` because 5paisa allows one feed
//! connection per token and evicts the other).
//!
//! One owned task polls every `interval` (clamped to 1..=60 s, web default
//! 5 s). The first poll seeds the snapshot silently; later polls publish an
//! `OrderUpdate` for every order whose `(status, filled quantity)` changed
//! or that is new. The diff is the shared one (`common::order_poll`). The snapshot is
//! rebuilt from each book, so it is bounded by the current order book.
//! Updates go into a bounded channel; when the receiver is dropped the task
//! ends. `OrderPoller::stop` and `Drop` abort the task.

use super::{orders, FivepaisaBroker};
use crate::brokers::common::order_poll::{clamp_interval, diff, CHANNEL_CAPACITY};
use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub use crate::brokers::common::order_poll::DEFAULT_INTERVAL;

pub struct OrderPoller {
    task: JoinHandle<()>,
}

impl OrderPoller {
    pub(crate) fn start(
        broker: FivepaisaBroker,
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
            let mut snapshot = None;
            loop {
                ticker.tick().await;
                if tx.is_closed() {
                    break;
                }
                let book = match orders::get_order_book(&broker, &auth).await {
                    Ok(b) => b,
                    Err(AppError::Auth(_)) => {
                        tracing::warn!("5paisa order updates stopped: the session has expired");
                        break;
                    }
                    Err(e) => {
                        tracing::debug!("5paisa order poll failed: {}", e.code());
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
