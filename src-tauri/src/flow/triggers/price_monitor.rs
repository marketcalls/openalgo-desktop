//! The price monitor's alerts (the web's `FlowPriceMonitor`,
//! `flow_price_monitor_service.py`): one alert per workflow, the condition
//! formulas on consecutive 5 s quotes, the one-shot latch, expiry, and the
//! claim that keeps one run at a time per alert. The poll loop and the run
//! live in `super` (they need the app); this file is the state, so the
//! tests drive it directly.
//!
//! An alert is replaced, never edited, by a new activation: each has its
//! own `gen`, and a queued run or a retirement acts only on the alert that
//! produced it (the web compared object identity).
//!
//! Departures from the web, each decided:
//!
//! - The channel bounds and the percentage are numbers at registration
//!   (T-031): the web kept an imported `"100"` as text and failed every
//!   poll comparing text with a price.
//! - An expired alert stands its workflow down too (T-038): the web only
//!   forgot the alert, so the workflow showed Active and a restart re-armed
//!   it with a fresh window.

use crate::flow::contract::pyfmt;
use crate::flow::node_contracts::normalize_condition;
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

/// The poll interval (`_poll_interval`).
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
/// Price-alert runs at once (`FLOW_PRICE_ALERT_WORKERS`).
pub const WORKERS: usize = 4;

/// One workflow's alert.
#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    /// This registration (a replacement gets a new one).
    pub gen: u64,
    pub workflow_id: i64,
    pub symbol: String,
    pub exchange: String,
    /// Canonical (`normalize_condition`).
    pub condition: String,
    pub target: f64,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub percentage: Option<f64>,
    /// The previous poll's price.
    pub last: Option<f64>,
    /// The one-shot latch.
    pub triggered: bool,
    pub created: DateTime<Utc>,
    /// `once` or `every_time` (lower-cased).
    pub trigger: String,
    /// `none`, `1h`, `4h`, `1d`, `1w`.
    pub expiration: String,
}

/// A level from the node: absent, or a number.
fn number(data: &Map<String, Value>, key: &str, label: &str) -> Result<Option<f64>, String> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(v) => pyfmt::to_float(v)
            .filter(|f| f.is_finite())
            .map(Some)
            .ok_or_else(|| format!("Price alert {} {} is not a number.", label, pyfmt::repr(v))),
    }
}

/// `str(v or default).strip().lower()`.
fn word(data: &Map<String, Value>, key: &str, default: &str) -> String {
    data.get(key)
        .filter(|v| pyfmt::truthy(v))
        .map(pyfmt::str)
        .unwrap_or_else(|| default.to_string())
        .trim()
        .to_lowercase()
}

impl Alert {
    /// The alert a priceAlert trigger node registers (`_register_trigger`,
    /// `add_alert`); the error answers activation 400.
    pub fn from_node(
        workflow_id: i64,
        data: &Map<String, Value>,
        now: DateTime<Utc>,
    ) -> Result<Alert, String> {
        let text = |k: &str, d: &str| match data.get(k) {
            None => d.to_string(),
            Some(v) => pyfmt::str(v),
        };
        let condition = data
            .get("condition")
            .map(pyfmt::str)
            .unwrap_or_else(|| "greater_than".into());
        Ok(Alert {
            gen: 0,
            workflow_id,
            symbol: text("symbol", ""),
            exchange: text("exchange", "NSE"),
            condition: normalize_condition(&condition),
            target: number(data, "price", "price")?.unwrap_or(0.0),
            lower: number(data, "priceLower", "lower price")?,
            upper: number(data, "priceUpper", "upper price")?,
            percentage: number(data, "percentage", "percentage")?,
            last: None,
            triggered: false,
            created: now,
            trigger: word(data, "trigger", "once"),
            expiration: word(data, "expiration", "none"),
        })
    }

    pub fn one_shot(&self) -> bool {
        self.trigger != "every_time"
    }

    /// Past its expiration window (`_is_expired`).
    pub fn expired(&self, now: DateTime<Utc>) -> bool {
        let window = match self.expiration.as_str() {
            "1h" => Duration::hours(1),
            "4h" => Duration::hours(4),
            "1d" => Duration::days(1),
            "1w" => Duration::weeks(1),
            _ => return false,
        };
        now - self.created >= window
    }

    /// The condition on this poll's price (`_evaluate_condition`); `None`
    /// for a condition no formula knows.
    pub fn evaluate(&self, ltp: f64) -> Option<bool> {
        let (target, last) = (self.target, self.last);
        let lower = self.lower.filter(|v| *v != 0.0).unwrap_or(target);
        let upper = self.upper.filter(|v| *v != 0.0).unwrap_or(target);
        let pct = self.percentage.unwrap_or(0.0);
        Some(match self.condition.as_str() {
            "greater_than" => ltp > target,
            "less_than" => ltp < target,
            "crossing" => (ltp - target).abs() <= ltp * 0.001,
            "crossing_up" => last.map_or(ltp > target, |l| l <= target && ltp > target),
            "crossing_down" => last.map_or(ltp < target, |l| l >= target && ltp < target),
            "entering_channel" | "inside_channel" => lower <= ltp && ltp <= upper,
            "exiting_channel" | "outside_channel" => ltp < lower || ltp > upper,
            "moving_up" => last.is_some_and(|l| ltp > l),
            "moving_down" => last.is_some_and(|l| ltp < l),
            "moving_up_percent" => last.is_some_and(|l| l != 0.0 && (ltp - l) / l * 100.0 >= pct),
            "moving_down_percent" => last.is_some_and(|l| l != 0.0 && (l - ltp) / l * 100.0 >= pct),
            _ => return None,
        })
    }
}

/// What a poll's price did to an alert.
#[derive(Debug, Clone, PartialEq)]
pub enum Observed {
    /// The condition is met: run the workflow at this price.
    Fire(Box<Alert>),
    /// Not met (the price is remembered for the next poll).
    Quiet,
    /// The alert was replaced or removed meanwhile.
    Gone,
}

/// Every workflow's alert.
#[derive(Default)]
pub struct Alerts {
    alerts: Mutex<HashMap<i64, Alert>>,
    /// Workflows with a run queued or running.
    pending: Mutex<HashSet<i64>>,
    gens: AtomicU64,
}

impl Alerts {
    /// Install `alert`, replacing the workflow's previous one; its `gen`.
    pub fn add(&self, mut alert: Alert) -> u64 {
        alert.gen = self.gens.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::info!(
            "Added price alert for workflow {}: {}@{} {} {}",
            alert.workflow_id,
            alert.symbol,
            alert.exchange,
            alert.condition,
            pyfmt::float(alert.target)
        );
        let gen = alert.gen;
        self.alerts.lock().insert(alert.workflow_id, alert);
        gen
    }

    pub fn remove(&self, workflow_id: i64) -> bool {
        let removed = self.alerts.lock().remove(&workflow_id).is_some();
        if removed {
            tracing::info!("Removed price alert for workflow {}", workflow_id);
        }
        removed
    }

    pub fn get(&self, workflow_id: i64) -> Option<Alert> {
        self.alerts.lock().get(&workflow_id).cloned()
    }

    pub fn len(&self) -> usize {
        self.alerts.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.alerts.lock().is_empty()
    }

    pub fn clear(&self) {
        self.alerts.lock().clear();
        self.pending.lock().clear();
    }

    /// Whether `gen` is still the workflow's alert.
    pub fn is_current(&self, workflow_id: i64, gen: u64) -> bool {
        self.alerts
            .lock()
            .get(&workflow_id)
            .is_some_and(|a| a.gen == gen)
    }

    /// The alerts a poll checks (latched one-shots are skipped).
    pub fn to_check(&self) -> Vec<Alert> {
        let mut v: Vec<Alert> = self
            .alerts
            .lock()
            .values()
            .filter(|a| !a.triggered)
            .cloned()
            .collect();
        v.sort_by_key(|a| a.workflow_id);
        v
    }

    /// Apply one poll's price (`_check_alert` after the quote): a met
    /// condition latches a one-shot alert before its run is queued, and an
    /// every-time alert remembers the price so a crossing needs a new cross.
    pub fn observe(&self, workflow_id: i64, gen: u64, ltp: f64) -> Observed {
        let mut alerts = self.alerts.lock();
        let Some(alert) = alerts.get_mut(&workflow_id).filter(|a| a.gen == gen) else {
            return Observed::Gone;
        };
        let met = match alert.evaluate(ltp) {
            Some(m) => m,
            None => {
                tracing::error!(
                    "Price alert for workflow {} has an unrecognized condition {}; it can never trigger.",
                    workflow_id,
                    pyfmt::quote(&alert.condition)
                );
                false
            }
        };
        if !met || alert.triggered {
            alert.last = Some(ltp);
            return Observed::Quiet;
        }
        if alert.one_shot() {
            alert.triggered = true;
        } else {
            alert.last = Some(ltp);
        }
        tracing::info!(
            "Price alert triggered for workflow {}: {}@{} {} (price: {}, target: {})",
            workflow_id,
            alert.symbol,
            alert.exchange,
            alert.condition,
            pyfmt::float(ltp),
            pyfmt::float(alert.target)
        );
        Observed::Fire(Box::new(alert.clone()))
    }

    /// Clear the latch of `gen` (its run did not execute).
    pub fn rearm(&self, workflow_id: i64, gen: u64) {
        if let Some(a) = self
            .alerts
            .lock()
            .get_mut(&workflow_id)
            .filter(|a| a.gen == gen)
        {
            a.triggered = false;
        }
    }

    /// Remove the alert if it is still `gen` (`_retire_one_shot`); `false`
    /// when it was replaced, which leaves the new one alone.
    pub fn retire(&self, workflow_id: i64, gen: u64) -> bool {
        let mut alerts = self.alerts.lock();
        if alerts.get(&workflow_id).is_some_and(|a| a.gen == gen) {
            alerts.remove(&workflow_id);
            tracing::info!("Removed price alert for workflow {}", workflow_id);
            return true;
        }
        tracing::debug!(
            "One-shot price alert for workflow {} was already replaced; leaving the current registration alone.",
            workflow_id
        );
        false
    }

    /// Claim the workflow's one pending run; `false` when one is already
    /// queued or running.
    pub fn claim_pending(&self, workflow_id: i64) -> bool {
        let claimed = self.pending.lock().insert(workflow_id);
        if !claimed {
            tracing::debug!(
                "Price alert for workflow {} already has a run queued; skipping this tick.",
                workflow_id
            );
        }
        claimed
    }

    pub fn release_pending(&self, workflow_id: i64) {
        self.pending.lock().remove(&workflow_id);
    }

    /// The web's `get_status()` without `running` and `poll_interval`.
    pub fn status(&self) -> Vec<Value> {
        let mut alerts: Vec<Alert> = self.alerts.lock().values().cloned().collect();
        alerts.sort_by_key(|a| a.workflow_id);
        alerts
            .iter()
            .map(|a| {
                json!({
                    "workflow_id": a.workflow_id,
                    "symbol": a.symbol,
                    "exchange": a.exchange,
                    "condition": a.condition,
                    "target_price": a.target,
                    "last_price": a.last,
                    "triggered": a.triggered,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
