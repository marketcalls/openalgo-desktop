//! Start-up: re-register every active workflow's trigger (the web's
//! `restore_order_update_watches`, `restore_price_alerts` and
//! `reconcile_scheduler_jobs`, run in that order from `app.py` 850-880,
//! each independent of the others).
//!
//! Schedules, alerts and watches live in memory; the active flag and the
//! next scheduled run are stored. The desktop has no persistent job store,
//! so every job is rebuilt from the active rows and an orphaned job cannot
//! exist (T-091). A schedule that fell due while the app was closed runs
//! once if the app starts within 60 s of it, else it is skipped (T-002).
//!
//! Departure from the web (T-047): a workflow whose trigger cannot be
//! restored (no symbol, an invalid watch or schedule) is stood down with
//! the reason logged, instead of staying Active with nothing watching.
//! The key is not needed here: a run reads the current key when it fires,
//! and the app starts signed out.

use super::store::ActiveRow;
use super::{order_monitor, price_monitor, scheduler, TriggerSide};
use serde_json::{Map, Value};

/// The data of the first node of `kind` in node order.
fn first(row: &ActiveRow, kind: &str) -> Option<Map<String, Value>> {
    row.nodes
        .as_array()?
        .iter()
        .find(|n| n.get("type").and_then(Value::as_str) == Some(kind))
        .map(|n| {
            n.get("data")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        })
}

impl TriggerSide {
    /// Re-register the active workflows' triggers.
    pub async fn restore(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let rows = match app
            .blocking(|c| super::store::active(&*c.sqlite.conn()?))
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!("Failed to restore Flow order-update watches: {}", e);
                tracing::error!("Failed to restore Flow price alerts: {}", e);
                tracing::error!("Failed to reconcile Flow scheduler jobs: {}", e);
                return;
            }
        };
        drop(app);
        self.restore_watches(&rows).await;
        self.restore_alerts(&rows).await;
        self.restore_schedules(&rows).await;
    }

    async fn restore_watches(&self, rows: &[ActiveRow]) {
        let mut restored = 0;
        for row in rows {
            let Some(data) = first(row, "orderUpdateTrigger") else {
                continue;
            };
            match order_monitor::Watch::from_node(row.id, &data) {
                Ok(w) => {
                    self.watches.add(w);
                    restored += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        "Skipping invalid order-update watch for workflow {}: {}",
                        row.id,
                        e
                    );
                    self.stand_down(
                        row.id,
                        &format!("Workflow {} was deactivated at start-up: its order-update trigger is not valid ({}).", row.id, e),
                    )
                    .await;
                }
            }
        }
        if restored > 0 {
            tracing::info!(
                "Restored {} order-update watch(es) from active workflows",
                restored
            );
        }
    }

    async fn restore_alerts(&self, rows: &[ActiveRow]) {
        let mut restored = 0;
        for row in rows {
            let Some(data) = first(row, "priceAlert") else {
                continue;
            };
            let symbol = data
                .get("symbol")
                .filter(|v| crate::flow::contract::pyfmt::truthy(v));
            let alert = match symbol {
                None => Err("no symbol configured".to_string()),
                Some(_) => price_monitor::Alert::from_node(row.id, &data, self.now()),
            };
            match alert {
                Ok(a) => {
                    self.alerts.add(a);
                    restored += 1;
                }
                Err(e) => {
                    tracing::warn!("Cannot restore price alert for workflow {}: {}", row.id, e);
                    self.stand_down(
                        row.id,
                        &format!("Workflow {} was deactivated at start-up: its price alert could not be restored ({}).", row.id, e),
                    )
                    .await;
                }
            }
        }
        if restored > 0 {
            tracing::info!("Restored {} price alert(s) from active workflows", restored);
            self.wake_prices.notify_one();
        }
    }

    async fn restore_schedules(&self, rows: &[ActiveRow]) {
        let mut restored = 0;
        let now = self.now();
        for row in rows {
            let Some(data) = first(row, "start") else {
                continue;
            };
            let schedule = match scheduler::Schedule::restored(&data, now) {
                Ok(Some(s)) => s,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        "Could not restore the scheduler job for active workflow {}; it will not fire until it is reactivated: {}",
                        row.id,
                        e
                    );
                    self.stand_down(
                        row.id,
                        &format!("Workflow {} was deactivated at start-up: its schedule is not valid ({}).", row.id, e),
                    )
                    .await;
                    continue;
                }
            };
            let gated = data
                .get("marketHoursOnly")
                .is_some_and(crate::flow::contract::pyfmt::truthy);
            let next = self
                .scheduler
                .restore(row.id, schedule, gated, row.next_run_at, now);
            if self.record_job(row.id, next).await.is_ok() {
                restored += 1;
            }
        }
        if restored > 0 {
            tracing::info!(
                "Scheduler reconciliation: removed 0 orphaned job(s), restored {} missing job(s)",
                restored
            );
            self.wake_scheduler.notify_one();
        }
    }
}
