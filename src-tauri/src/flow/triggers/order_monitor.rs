//! The order-update monitor's watches (the web's `FlowOrderUpdateMonitor`,
//! `flow_order_update_monitor_service.py`): one watch per workflow, matched
//! against every `order.update` the event bus delivers on its critical
//! lane, claimed under the lock so a one-time watch never fires twice. The
//! bus subscriber and the run live in `super`; this file is the state.
//!
//! The desktop has one account, so every order update is the signed-in
//! trader's: the web's owner check (`request_data.user_id`) has nothing to
//! tell apart (parity T-052). Sandbox fills publish the same event, so a
//! workflow in analyzer mode fires as on the web.
//!
//! Departure from the web, decided (T-057): an every-time watch whose run
//! is still going keeps the newest update that arrived meanwhile and runs
//! once more for it, with a log line, instead of dropping it silently (a
//! basket filling leg by leg ran the workflow for the first fill only).

use crate::events::OrderUpdate;
use crate::flow::contract::pyfmt;
use crate::flow::node_contracts::{normalize_status, VALID_STATUSES};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Order-update runs at once (`FLOW_ORDER_UPDATE_WORKERS`).
pub const WORKERS: usize = 4;

/// One workflow's watch.
#[derive(Debug, Clone)]
pub struct Watch {
    pub gen: u64,
    pub workflow_id: i64,
    pub order_id: Option<String>,
    pub symbol: Option<String>,
    pub exchange: Option<String>,
    /// Normalized (`normalize_status`): `any`, `open`, `trigger pending`,
    /// `complete`, `rejected`, `cancelled`.
    pub status: String,
    /// `once` or `every_time`.
    pub trigger: String,
    /// Claimed: a run is queued or going (a one-time watch stays claimed).
    pub triggered: bool,
    /// The newest update an every-time watch received while claimed.
    pub followup: Option<OrderUpdate>,
}

/// `value or None` as text.
fn opt(data: &Map<String, Value>, key: &str) -> Option<String> {
    data.get(key).filter(|v| pyfmt::truthy(v)).map(pyfmt::str)
}

impl Watch {
    /// The watch an orderUpdateTrigger node registers (`add_watch`); the
    /// error is the web's sentence, answered 400 by activation.
    pub fn from_node(workflow_id: i64, data: &Map<String, Value>) -> Result<Watch, String> {
        let order_id = opt(data, "orderId");
        let symbol = opt(data, "symbol");
        if order_id.is_none() && symbol.is_none() {
            return Err("orderUpdateTrigger needs an Order ID or a Symbol to watch".into());
        }
        if order_id.as_deref().is_some_and(|o| o.contains("{{")) {
            return Err("orderUpdateTrigger Order ID must be a literal order id, not a {{variable}} reference - a trigger node has no upstream node to resolve it from. Leave it blank and filter by Symbol instead.".into());
        }
        let raw = data.get("status").cloned().unwrap_or(json!("complete"));
        let status = normalize_status(&pyfmt::str(&raw));
        if !VALID_STATUSES.contains(&status.as_str()) {
            let mut valid: Vec<&str> = VALID_STATUSES.to_vec();
            valid.sort_unstable();
            let list = Value::Array(valid.iter().map(|s| json!(s)).collect());
            return Err(format!(
                "orderUpdateTrigger status must be one of {}, got {}",
                pyfmt::repr(&list),
                pyfmt::repr(&raw)
            ));
        }
        let trigger = data
            .get("trigger")
            .map(pyfmt::str)
            .unwrap_or_else(|| "once".into());
        Ok(Watch {
            gen: 0,
            workflow_id,
            order_id,
            symbol,
            exchange: opt(data, "exchange"),
            status,
            trigger,
            triggered: false,
            followup: None,
        })
    }

    pub fn one_shot(&self) -> bool {
        self.trigger != "every_time"
    }

    /// Whether `u` is an update this watch fires on (`_matches`).
    pub fn matches(&self, u: &OrderUpdate) -> bool {
        (self.status == "any" || normalize_status(&u.order_status) == self.status)
            && self.order_id.as_deref().is_none_or(|o| o == u.orderid)
            && self.symbol.as_deref().is_none_or(|s| s == u.symbol)
            && self.exchange.as_deref().is_none_or(|e| e == u.exchange)
    }
}

/// What a watch does after its run.
#[derive(Debug, Clone)]
pub enum After {
    /// A one-time watch whose run executed: retire it and stand the
    /// workflow down.
    Retire,
    /// Its run did not execute: the watch fires again on the next update.
    Rearmed,
    /// An every-time watch with an update that came during the run: run
    /// again for it.
    Again(Box<OrderUpdate>),
    /// Nothing more (every-time, or the watch was replaced).
    Done,
}

/// Every workflow's watch.
#[derive(Default)]
pub struct Watches {
    watches: Mutex<HashMap<i64, Watch>>,
    gens: AtomicU64,
    /// Off between a shutdown and the next start (updates are ignored).
    attached: AtomicBool,
}

impl Watches {
    /// Install `watch`, replacing the workflow's previous one; its `gen`.
    pub fn add(&self, mut watch: Watch) -> u64 {
        watch.gen = self.gens.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::info!(
            "Added order-update watch for workflow {}: orderid={} symbol={} status={}",
            watch.workflow_id,
            watch.order_id.as_deref().unwrap_or("None"),
            watch.symbol.as_deref().unwrap_or("None"),
            watch.status
        );
        let gen = watch.gen;
        self.watches.lock().insert(watch.workflow_id, watch);
        gen
    }

    pub fn remove(&self, workflow_id: i64) -> bool {
        self.watches.lock().remove(&workflow_id).is_some()
    }

    pub fn get(&self, workflow_id: i64) -> Option<Watch> {
        self.watches.lock().get(&workflow_id).cloned()
    }

    pub fn is_current(&self, workflow_id: i64, gen: u64) -> bool {
        self.watches
            .lock()
            .get(&workflow_id)
            .is_some_and(|w| w.gen == gen)
    }

    /// Receive updates (start-up, and after a shutdown).
    pub fn attach(&self) {
        self.attached.store(true, Ordering::SeqCst);
    }

    /// Stop receiving and forget every watch (`shutdown`).
    pub fn detach(&self) {
        self.attached.store(false, Ordering::SeqCst);
        self.watches.lock().clear();
    }

    pub fn clear(&self) {
        self.watches.lock().clear();
    }

    /// Claim every unclaimed watch `u` matches, under the lock
    /// (`_on_order_update`). A claimed every-time watch keeps `u` for one
    /// more run.
    pub fn claim(&self, u: &OrderUpdate) -> Vec<(i64, u64)> {
        if !self.attached.load(Ordering::SeqCst) {
            return Vec::new();
        }
        let mut claimed = Vec::new();
        for w in self.watches.lock().values_mut() {
            if !w.matches(u) {
                continue;
            }
            if !w.triggered {
                w.triggered = true;
                claimed.push((w.workflow_id, w.gen));
            } else if !w.one_shot() {
                tracing::info!(
                    "Order-update trigger for workflow {} is still running; it runs again for order {} ({}) when this run ends.",
                    w.workflow_id,
                    u.orderid,
                    u.order_status
                );
                w.followup = Some(u.clone());
            }
        }
        claimed.sort_unstable();
        claimed
    }

    /// The watch after its run (`executed`: the run reached the graph).
    pub fn after_run(&self, workflow_id: i64, gen: u64, executed: bool) -> After {
        let mut watches = self.watches.lock();
        let Some(w) = watches.get_mut(&workflow_id).filter(|w| w.gen == gen) else {
            tracing::debug!(
                "One-shot order-update watch for workflow {} was already replaced; leaving the current registration alone.",
                workflow_id
            );
            return After::Done;
        };
        if w.one_shot() {
            if executed {
                watches.remove(&workflow_id);
                tracing::info!("Removed order-update watch for workflow {}", workflow_id);
                return After::Retire;
            }
            w.triggered = false;
            tracing::info!(
                "Re-arming one-shot order-update watch for workflow {}: the run did not execute.",
                workflow_id
            );
            return After::Rearmed;
        }
        match w.followup.take() {
            Some(next) => After::Again(Box::new(next)),
            None => {
                w.triggered = false;
                After::Done
            }
        }
    }

    /// The web's `get_status()`.
    pub fn status(&self) -> Value {
        let mut watches: Vec<Watch> = self.watches.lock().values().cloned().collect();
        watches.sort_by_key(|w| w.workflow_id);
        json!({
            "watches_count": watches.len(),
            "watches": watches.iter().map(|w| json!({
                "workflow_id": w.workflow_id,
                "order_id": w.order_id,
                "symbol": w.symbol,
                "exchange": w.exchange,
                "status": w.status,
                "triggered": w.triggered,
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests;
