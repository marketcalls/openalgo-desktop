//! Records OpenAlgo meant to save and did not (OBS-01, OBS-02).
//!
//! Order and analyzer log entries ride the event bus's best-effort lane,
//! which sheds when the log subscriber's queue is full, exactly as the web's
//! `utils/event_bus.py` does; a write can also fail. Traffic, latency and
//! security records ride the monitor's bounded queue. The web loses such
//! records silently. Here the counts are shown on the Health page
//! (`/health/api/current`, `records`) and in the system report, and a new
//! loss raises a health alert that names it.

use crate::db::sqlite::monitor::{self as store, AlertRow};
use crate::error::Result;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

/// The alert a loss raises; the trader resolves it.
pub const ALERT: &str = "records_not_saved";

/// What was lost since start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Losses {
    /// Order and analyzer log entries shed by the log subscriber's queue.
    pub order_logs_dropped: u64,
    /// ... and those whose write failed.
    pub order_logs_failed: u64,
    /// Monitoring records shed by the monitor's queue.
    pub monitoring_dropped: u64,
    /// ... and those its writer could not save.
    pub monitoring_lost: u64,
}

impl Losses {
    pub fn order_logs(&self) -> u64 {
        self.order_logs_dropped + self.order_logs_failed
    }

    pub fn monitoring(&self) -> u64 {
        self.monitoring_dropped + self.monitoring_lost
    }

    pub fn total(&self) -> u64 {
        self.order_logs() + self.monitoring()
    }
}

pub fn losses(ctx: &AppState) -> Losses {
    let log_queue = ctx
        .bus
        .queue_depths()
        .into_iter()
        .find(|q| q.name == crate::events::subscribers::log::NAME);
    Losses {
        order_logs_dropped: log_queue.map(|q| q.dropped).unwrap_or(0),
        order_logs_failed: ctx.logs.failed_log_writes(),
        monitoring_dropped: ctx.monitor.dropped(),
        monitoring_lost: ctx.monitor.lost(),
    }
}

/// The counts for health and the system report, with every subscriber's
/// queue (its depth and what it shed).
pub fn json(ctx: &AppState) -> Value {
    let l = losses(ctx);
    let queues: Vec<Value> = ctx
        .bus
        .queue_depths()
        .into_iter()
        .map(|q| {
            json!({
                "name": q.name,
                "lane": match q.lane {
                    crate::events::Lane::BestEffort => "best_effort",
                    crate::events::Lane::Critical => "critical",
                },
                "depth": q.depth,
                "capacity": q.capacity,
                "dropped": q.dropped,
            })
        })
        .collect();
    json!({
        "status": if l.total() == 0 { "pass" } else { "warn" },
        "order_logs_dropped": l.order_logs_dropped,
        "order_logs_failed": l.order_logs_failed,
        "monitoring_dropped": l.monitoring_dropped,
        "monitoring_lost": l.monitoring_lost,
        "monitoring_retried_batches": ctx.monitor.retried(),
        "subscribers": queues,
    })
}

/// Raise the alert when records were lost since the last alert. It is not
/// resolved automatically: the counts only grow until OpenAlgo restarts,
/// and the trader resolves it once read.
pub fn alert(conn: &Connection, ctx: &AppState, now: DateTime<Utc>) -> Result<()> {
    let l = losses(ctx);
    let total = l.total();
    if total <= ctx.monitor.loss_alerted.load(Ordering::Relaxed) {
        return Ok(());
    }
    store::raise_alert(
        conn,
        &AlertRow {
            alert_type: ALERT.into(),
            severity: "warn".into(),
            metric_name: ALERT.into(),
            metric_value: total as f64,
            threshold_value: 0.0,
            message: message(&l),
            ..Default::default()
        },
        now,
    )?;
    ctx.monitor.loss_alerted.store(total, Ordering::Relaxed);
    Ok(())
}

fn message(l: &Losses) -> String {
    let mut parts = Vec::new();
    if l.order_logs() > 0 {
        parts.push(format!(
            "{} order log entries (Logs and Analyzer pages)",
            l.order_logs()
        ));
    }
    if l.monitoring() > 0 {
        parts.push(format!(
            "{} monitoring records (Traffic, Latency and Security pages)",
            l.monitoring()
        ));
    }
    format!(
        "OpenAlgo could not save {} since it started. Orders are not affected; those pages are \
         missing the entries. If this keeps happening, restart OpenAlgo and report it.",
        parts.join(" and ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_names_what_was_lost() {
        let l = Losses {
            order_logs_dropped: 3,
            order_logs_failed: 1,
            ..Default::default()
        };
        let m = message(&l);
        assert!(m.contains("4 order log entries"), "{}", m);
        assert!(!m.contains("monitoring"), "{}", m);
        let l = Losses {
            monitoring_lost: 2,
            ..l
        };
        assert!(message(&l).contains("and 2 monitoring records"));
    }
}
