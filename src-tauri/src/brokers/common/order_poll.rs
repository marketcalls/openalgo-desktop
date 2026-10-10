//! Order updates by polling the order book, shared by brokers without an
//! order socket (web `websocket_proxy/order_adapter.py`
//! `PollingOrderUpdateAdapter` and `_POLLING_BROKERS`: Groww, 5paisa,
//! Samco).
//!
//! `OrderPoller` is the one polling loop: one owned task reads the order
//! book every `interval` (clamped to 1..=60 s, web default 5 s). The first
//! poll seeds the snapshot silently; later polls yield an `OrderUpdate` for
//! every order whose `(status, filled quantity)` changed or that is new.
//! The snapshot is rebuilt from each book, so it is bounded by the current
//! order book. Updates go into a bounded channel; when the receiver is
//! dropped the task ends. A failed read never stops the poller unless the
//! adapter says the session is over (`PollError::SessionEnded`): anything
//! else, an IP refusal included, backs off (doubling up to
//! `MAX_BACKOFF`) and polls again, so updates resume by themselves once
//! the cause is fixed. `OrderPoller::stop` and `Drop` abort the task; the
//! adapter keeps it and stops it in `Broker::on_logout`.

use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::types::Order;
use crate::error::{AppError, Result};
use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
pub const MAX_INTERVAL: Duration = Duration::from_secs(60);
/// The longest wait between polls after repeated failures.
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// Updates buffered for a slow consumer before the poller waits.
pub const CHANNEL_CAPACITY: usize = 256;

pub fn clamp_interval(d: Duration) -> Duration {
    d.clamp(MIN_INTERVAL, MAX_INTERVAL)
}

/// The wait before the next poll after `failures` failed reads in a row:
/// the interval, doubled per failure, capped at `MAX_BACKOFF`.
pub fn backoff(interval: Duration, failures: u32) -> Duration {
    let factor = 1u32 << failures.min(16);
    interval.saturating_mul(factor).min(MAX_BACKOFF)
}

/// What a failed order-book read means to the poller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollError {
    /// The broker confirmed the session is over: stop polling (a new
    /// sign-in starts a new poller).
    SessionEnded,
    /// Anything else (network, server error, rate limit, IP refusal): back
    /// off and poll again.
    Retry,
}

/// The usual reading: an authentication refusal ends the session.
pub fn session_ends_on_auth(e: &AppError) -> PollError {
    match e {
        AppError::Auth(_) => PollError::SessionEnded,
        _ => PollError::Retry,
    }
}

/// The running order-book poller of one broker session.
pub struct OrderPoller {
    task: JoinHandle<()>,
}

impl OrderPoller {
    /// Start polling with `fetch` (one order-book read) every `interval`.
    /// `classify` reads a failed read (`session_ends_on_auth` for most
    /// brokers). `broker` names the broker in logs.
    pub fn start<F, Fut>(
        broker: &'static str,
        interval: Duration,
        fetch: F,
        classify: fn(&AppError) -> PollError,
    ) -> Result<(Self, mpsc::Receiver<OrderUpdate>)>
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<Order>>> + Send + 'static,
    {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| AppError::Internal("Order updates need the async runtime".into()))?;
        let interval = clamp_interval(interval);
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let task = handle.spawn(async move {
            let mut snapshot: Option<Snapshot> = None;
            let mut failures: u32 = 0;
            loop {
                if tx.is_closed() {
                    return;
                }
                match fetch().await {
                    Ok(book) => {
                        if failures > 0 {
                            tracing::info!(broker, "Order updates resumed");
                        }
                        failures = 0;
                        let (next, changed) = diff(snapshot.as_ref(), &book);
                        snapshot = Some(next);
                        for u in changed {
                            if tx.send(u).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => match classify(&e) {
                        PollError::SessionEnded => {
                            tracing::warn!(broker, "Order updates stopped: the session has ended");
                            return;
                        }
                        PollError::Retry => {
                            failures = failures.saturating_add(1);
                            if failures == 1 || failures.is_power_of_two() {
                                tracing::warn!(
                                    broker,
                                    failures,
                                    "Order book poll failed, retrying: {}",
                                    e.code()
                                );
                            }
                        }
                    },
                }
                tokio::time::sleep(backoff(interval, failures)).await;
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

/// Normalised order update from an order-book row.
pub fn to_update(o: &Order) -> OrderUpdate {
    OrderUpdate {
        orderid: o.order_id.clone(),
        symbol: o.symbol.clone(),
        exchange: o.exchange.clone(),
        action: o.side.clone(),
        quantity: i64::from(o.quantity),
        price: o.price,
        trigger_price: o.trigger_price,
        pricetype: o.order_type.clone(),
        product: o.product.clone(),
        order_status: o.status.clone(),
        filled_quantity: i64::from(o.filled_quantity),
        pending_quantity: i64::from(o.pending_quantity),
        average_price: o.average_price,
        rejection_reason: o.rejection_reason.clone().unwrap_or_default(),
    }
}

pub type Snapshot = HashMap<String, (String, i32)>;

/// Changes between two polls; returns the new snapshot.
pub fn diff(previous: Option<&Snapshot>, book: &[Order]) -> (Snapshot, Vec<OrderUpdate>) {
    let mut next = Snapshot::with_capacity(book.len());
    let mut changed = Vec::new();
    for o in book {
        let state = (o.status.clone(), o.filled_quantity);
        if let Some(prev) = previous {
            if prev.get(&o.order_id) != Some(&state) {
                changed.push(to_update(o));
            }
        }
        next.insert(o.order_id.clone(), state);
    }
    (next, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::VecDeque;
    use std::sync::Arc;

    fn order(id: &str, status: &str, filled: i32) -> Order {
        Order {
            order_id: id.into(),
            exchange_order_id: None,
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            side: "BUY".into(),
            quantity: 10,
            filled_quantity: filled,
            pending_quantity: 10 - filled,
            price: 800.0,
            trigger_price: 0.0,
            average_price: 0.0,
            order_type: "LIMIT".into(),
            product: "MIS".into(),
            status: status.into(),
            validity: "DAY".into(),
            order_timestamp: String::new(),
            exchange_timestamp: None,
            rejection_reason: None,
            order_tag: None,
        }
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let i = Duration::from_secs(5);
        assert_eq!(backoff(i, 0), i);
        assert_eq!(backoff(i, 1), Duration::from_secs(10));
        assert_eq!(backoff(i, 3), Duration::from_secs(40));
        assert_eq!(backoff(i, 7), MAX_BACKOFF);
        assert_eq!(backoff(i, u32::MAX), MAX_BACKOFF);
    }

    /// BF-01: the one polling loop. The first read seeds silently, a change
    /// is published once, a failed read (any refusal but a confirmed
    /// session end) backs off and polling goes on, and a confirmed session
    /// end stops it.
    #[tokio::test(start_paused = true)]
    async fn a_failed_read_backs_off_and_only_a_session_end_stops() {
        type Read = Result<Vec<Order>>;
        let script: Arc<Mutex<VecDeque<Read>>> = Arc::new(Mutex::new(VecDeque::from(vec![
            Ok(vec![order("1", "open", 0)]),
            Err(AppError::Broker("down".into())),
            Err(AppError::Auth("refused from this IP".into())),
            Ok(vec![order("1", "complete", 10)]),
            Ok(vec![order("1", "complete", 10)]),
            Err(AppError::Auth("session expired".into())),
        ])));
        let reads = Arc::new(Mutex::new(0u32));
        let (s, r) = (script.clone(), reads.clone());
        let classify: fn(&AppError) -> PollError = |e| match e {
            AppError::Auth(m) if m == "session expired" => PollError::SessionEnded,
            _ => PollError::Retry,
        };
        let (poller, mut rx) = OrderPoller::start(
            "test",
            Duration::from_secs(1),
            move || {
                *r.lock() += 1;
                let next = s.lock().pop_front();
                async move { next.unwrap_or_else(|| Ok(Vec::new())) }
            },
            classify,
        )
        .unwrap();
        let u = rx.recv().await.expect("the fill");
        assert_eq!(
            (u.orderid.as_str(), u.order_status.as_str()),
            ("1", "complete")
        );
        // The stream ends at the confirmed session end, with nothing more.
        assert!(rx.recv().await.is_none());
        assert_eq!(*reads.lock(), 6);
        drop(poller);
    }

    /// `session_ends_on_auth`, the reading for brokers whose refusals are
    /// unambiguous.
    #[test]
    fn auth_ends_the_session_by_default() {
        assert_eq!(
            session_ends_on_auth(&AppError::Auth("x".into())),
            PollError::SessionEnded
        );
        assert_eq!(
            session_ends_on_auth(&AppError::Broker("x".into())),
            PollError::Retry
        );
    }
}
