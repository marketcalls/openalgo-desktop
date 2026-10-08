//! Order updates by polling the order book, shared by brokers without an
//! order socket (web `websocket_proxy/order_adapter.py`
//! `PollingOrderUpdateAdapter`: Groww, 5paisa).
//!
//! The pieces every poller uses: the interval bounds, the channel bound and
//! the diff between two books. The first poll seeds the snapshot silently;
//! later polls yield an `OrderUpdate` for every order whose
//! `(status, filled quantity)` changed or that is new. The snapshot is
//! rebuilt from each book, so it is bounded by the current order book.

use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::types::Order;
use std::collections::HashMap;
use std::time::Duration;

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
pub const MAX_INTERVAL: Duration = Duration::from_secs(60);
/// Updates buffered for a slow consumer before the poller waits.
pub const CHANNEL_CAPACITY: usize = 256;

pub fn clamp_interval(d: Duration) -> Duration {
    d.clamp(MIN_INTERVAL, MAX_INTERVAL)
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
