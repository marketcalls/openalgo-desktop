//! What runs the triggers: the scheduler driver (`execute_workflow_scheduled`),
//! the price monitor's 5 s poll (`_monitoring_loop`, `_check_alert`,
//! `_trigger_workflow`) and the order-update subscriber on the event bus's
//! critical lane (`_on_order_update`, `_fire`). Each run is an owned task,
//! at most four at once per monitor (the web's worker pools); a run never
//! blocks the driver, the poll or the bus.

use super::order_monitor::After;
use super::price_monitor::{Alert, Observed};
use super::runs::Fired;
use super::scheduler::Due;
use super::{market_hours, store, TriggerSide};
use crate::events::{Event, Lane, OrderUpdate, Subscriber, Topic};
use crate::flow::contract::client::app::AppFlowClient;
use crate::flow::contract::client::FlowClient;
use crate::flow::contract::node::RunTrigger;
use crate::flow::contract::pyfmt;
use crate::state::AppState;
use serde_json::{json, Value};
use std::sync::{Arc, Weak};

impl TriggerSide {
    /// The scheduler's driver: fire what is due, sleep until the next fire
    /// (at most a minute, so a clock change is noticed).
    pub async fn drive_schedules(self: Arc<Self>) {
        // Ends with the app too (a context dropped without its shutdown).
        while self.app().is_some() {
            let now = self.now();
            for due in self.scheduler.tick(now) {
                self.persist_next(&due).await;
                let me = self.clone();
                self.spawn(async move { me.run_scheduled(due).await });
            }
            let sleep = self.scheduler.sleep_for(self.now());
            tokio::select! {
                _ = tokio::time::sleep(sleep) => {}
                _ = self.wake_scheduler.notified() => {}
                _ = self.stop.cancelled() => return,
            }
        }
    }

    /// Keep the recorded next run in step with the job.
    async fn persist_next(&self, due: &Due) {
        let Some(app) = self.app() else {
            return;
        };
        let (id, next) = (due.workflow_id, due.next);
        let saved = app
            .blocking(move |c| store::set_next_run(&*c.sqlite.conn()?, id, next))
            .await;
        if let Err(e) = saved {
            tracing::error!("Could not record the next run of workflow {}: {}", id, e);
        }
    }

    /// One scheduled fire (`execute_workflow_scheduled`): the workflow must
    /// still exist and be active, the market-hours gate is read from the
    /// graph each time, and a one-time schedule stands its workflow down
    /// after it fired or was missed (T-010).
    pub async fn run_scheduled(&self, due: Due) {
        let id = due.workflow_id;
        if due.missed {
            tracing::warn!(
                "Scheduled run of workflow {} was missed by more than {} s; skipping it",
                id,
                super::scheduler::MISFIRE_GRACE_SECS
            );
        } else {
            tracing::info!("Scheduled execution of workflow {}", id);
            self.scheduled_run(id, due.market_hours_only).await;
        }
        if due.once {
            let why = if due.missed {
                "was missed"
            } else {
                "has fired"
            };
            self.stand_down(
                id,
                &format!(
                    "The one-time schedule of workflow {} {}; the workflow was deactivated so it cannot run again.",
                    id, why
                ),
            )
            .await;
        }
    }

    pub(crate) async fn scheduled_run(&self, id: i64, stored_gate: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let row = app
            .blocking(move |c| store::graph(&*c.sqlite.conn()?, id))
            .await;
        let hours = match row {
            Ok(None) => {
                tracing::warn!("Skipping scheduled workflow {}: it no longer exists", id);
                return;
            }
            Ok(Some((false, _))) => {
                tracing::warn!("Skipping scheduled workflow {}: it is not active", id);
                return;
            }
            Ok(Some((true, nodes))) => market_hours::config(&nodes),
            Err(e) => {
                tracing::error!(
                    "Could not read scheduling config for workflow {}; using the schedule's stored setting: {}",
                    id,
                    e
                );
                market_hours::MarketHours {
                    enabled: stored_gate,
                    ..Default::default()
                }
            }
        };
        if hours.enabled && !self.within_market_hours(&app, &hours).await {
            tracing::debug!("Skipping scheduled workflow {}: outside market hours", id);
            return;
        }
        let fired = self.fire(id, RunTrigger::Schedule, None).await;
        tracing::info!(
            "Scheduled execution result for workflow {}: {}",
            id,
            fired.status()
        );
    }

    /// The gate on the desktop calendar; a calendar that cannot be read
    /// keeps the run out (fail closed).
    async fn within_market_hours(
        &self,
        app: &Arc<AppState>,
        hours: &market_hours::MarketHours,
    ) -> bool {
        let now = crate::flow::contract::time::ist_now(self.clock.as_ref());
        let date = now.date();
        let windows = app
            .blocking(move |c| crate::services::market_calendar_service::timings_for(c, date))
            .await;
        match windows {
            Ok(w) => market_hours::is_within(now, hours, |_, ex| market_hours::window_of(&w, ex)),
            Err(e) => {
                tracing::error!("Could not read the market calendar for a schedule: {}", e);
                false
            }
        }
    }

    /// The price monitor's poll: every 5 s while there are alerts, idle
    /// otherwise.
    pub async fn poll_prices(self: Arc<Self>) {
        let mut paused = false;
        while self.app().is_some() {
            if self.alerts.is_empty() {
                tokio::select! {
                    _ = self.wake_prices.notified() => continue,
                    _ = self.stop.cancelled() => return,
                }
            }
            paused = self.check_prices(paused).await;
            tokio::select! {
                _ = tokio::time::sleep(super::price_monitor::POLL_INTERVAL) => {}
                _ = self.stop.cancelled() => return,
            }
        }
    }

    /// One poll over every alert (`_check_all_alerts`); `true` while it is
    /// paused because OpenAlgo is signed out (logged once per pause). With
    /// no broker logged in the quotes fail and nothing fires, as on the web.
    pub(crate) async fn check_prices(self: &Arc<Self>, was_paused: bool) -> bool {
        let Some(app) = self.app() else {
            return was_paused;
        };
        if let Some(reason) = super::runs::paused(&app, RunTrigger::PriceAlert) {
            if !was_paused {
                tracing::info!("Flow price alerts are not checked for now. {}", reason);
            }
            return true;
        }
        let Some(key) = crate::flow::service::api_key(&app).await else {
            for a in self.alerts.to_check() {
                tracing::warn!("No API key for alert workflow {}", a.workflow_id);
            }
            return was_paused;
        };
        let client = AppFlowClient::new(Arc::downgrade(&app), key);
        drop(app);
        for alert in self.alerts.to_check() {
            self.check_alert(&client, alert).await;
        }
        false
    }

    /// One alert (`_check_alert`).
    async fn check_alert(self: &Arc<Self>, client: &AppFlowClient, alert: Alert) {
        let id = alert.workflow_id;
        if alert.expired(self.now()) {
            tracing::info!(
                "Price alert for workflow {} expired after {}; no longer watching.",
                id,
                alert.expiration
            );
            if self.alerts.retire(id, alert.gen) {
                self.stand_down(
                    id,
                    &format!(
                        "Price alert for workflow {} expired after {}; the workflow was deactivated.",
                        id, alert.expiration
                    ),
                )
                .await;
            }
            return;
        }
        let quote = client.get_quotes(&alert.symbol, &alert.exchange).await;
        if quote.get("status").and_then(Value::as_str) != Some("success") {
            tracing::debug!(
                "Failed to get quote for {}: {}",
                alert.symbol,
                pyfmt::repr(&quote)
            );
            return;
        }
        let ltp = quote
            .get("data")
            .and_then(|d| d.get("ltp"))
            .and_then(pyfmt::to_float)
            .unwrap_or(0.0);
        if ltp <= 0.0 {
            return;
        }
        if let Observed::Fire(fired) = self.alerts.observe(id, alert.gen, ltp) {
            self.queue_price_run(*fired, ltp);
        }
    }

    /// Queue one run for an alert that fired, at most one per workflow
    /// (`_trigger_workflow`).
    pub fn queue_price_run(self: &Arc<Self>, alert: Alert, price: f64) {
        if !self.alerts.claim_pending(alert.workflow_id) {
            return;
        }
        let me = self.clone();
        let permits = self.price_runs.clone();
        self.spawn(async move {
            let Ok(_permit) = permits.acquire_owned().await else {
                return;
            };
            let fired = me.run_price_alert(&alert, price).await;
            let id = alert.workflow_id;
            if alert.one_shot() {
                if fired.executed() {
                    if me.alerts.retire(id, alert.gen) {
                        me.stand_down(
                            id,
                            &format!("One-shot price alert for workflow {} consumed; workflow deactivated.", id),
                        )
                        .await;
                    }
                } else {
                    me.alerts.rearm(id, alert.gen);
                    tracing::info!(
                        "Re-arming one-shot price alert for workflow {}: the run did not execute.",
                        id
                    );
                }
            }
            me.alerts.release_pending(id);
        });
    }

    async fn run_price_alert(&self, alert: &Alert, price: f64) -> Fired {
        let id = alert.workflow_id;
        if !self.alerts.is_current(id, alert.gen) {
            tracing::info!(
                "Dropping queued price-alert run for workflow {}: the alert was removed or replaced before this run claimed it.",
                id
            );
            return Fired::Skipped;
        }
        if !self.still_active(id).await {
            tracing::info!(
                "Dropping queued price-alert run for workflow {}: the workflow is no longer active.",
                id
            );
            return Fired::Skipped;
        }
        let payload = json!({
            "trigger_type": "price_alert",
            "trigger_price": price,
            "triggered_at": self.triggered_at(),
        });
        let fired = self.fire(id, RunTrigger::PriceAlert, Some(payload)).await;
        tracing::info!("Workflow {} execution result: {}", id, fired.status());
        fired
    }

    /// An order update from the bus: claim every matching watch and queue
    /// its run (`_on_order_update`); never blocks the bus.
    pub fn on_order_update(self: &Arc<Self>, update: &OrderUpdate) {
        for (id, gen) in self.watches.claim(update) {
            tracing::info!(
                "Order-update trigger fired for workflow {}: orderid={} status={}",
                id,
                update.orderid,
                update.order_status
            );
            let me = self.clone();
            let permits = self.order_runs.clone();
            let first = update.clone();
            self.spawn(async move {
                let Ok(_permit) = permits.acquire_owned().await else {
                    return;
                };
                me.order_runs_for(id, gen, first).await;
            });
        }
    }

    /// Run for `update`, then for each follow-up an every-time watch kept,
    /// then retire a spent one-time watch.
    async fn order_runs_for(&self, id: i64, gen: u64, mut update: OrderUpdate) {
        loop {
            let fired = self.run_order_update(id, gen, &update).await;
            match self.watches.after_run(id, gen, fired.executed()) {
                After::Again(next) => update = *next,
                After::Retire => {
                    self.stand_down(
                        id,
                        &format!("One-shot order-update watch for workflow {} consumed; workflow deactivated.", id),
                    )
                    .await;
                    return;
                }
                After::Rearmed | After::Done => return,
            }
        }
    }

    async fn run_order_update(&self, id: i64, gen: u64, u: &OrderUpdate) -> Fired {
        if !self.watches.is_current(id, gen) {
            tracing::info!(
                "Dropping queued order-update run for workflow {}: the watch was removed or replaced before this run claimed it.",
                id
            );
            return Fired::Skipped;
        }
        if !self.still_active(id).await {
            tracing::info!(
                "Dropping queued order-update run for workflow {}: the workflow is no longer active.",
                id
            );
            return Fired::Skipped;
        }
        let payload = json!({
            "trigger_type": "order_update",
            "orderid": u.orderid,
            "symbol": u.symbol,
            "exchange": u.exchange,
            "order_status": u.order_status,
            "filled_quantity": u.filled_quantity,
            "average_price": u.average_price,
            "rejection_reason": u.rejection_reason,
            "triggered_at": self.triggered_at(),
        });
        let fired = self.fire(id, RunTrigger::OrderUpdate, Some(payload)).await;
        tracing::info!("Workflow {} execution result: {}", id, fired.status());
        fired
    }
}

/// The bus subscriber (`FlowOrderUpdateMonitor`, critical lane: a fill the
/// trigger never hears about is a stop leg never placed).
struct FlowOrderUpdates {
    side: Weak<TriggerSide>,
}

#[async_trait::async_trait]
impl Subscriber for FlowOrderUpdates {
    fn name(&self) -> &'static str {
        "FlowOrderUpdateMonitor"
    }

    fn topics(&self) -> Vec<Topic> {
        vec![Topic::OrderUpdate]
    }

    async fn handle(&self, event: Arc<Event>) {
        let Event::OrderUpdate(u) = &*event else {
            return;
        };
        if let Some(side) = self.side.upgrade() {
            side.on_order_update(u);
        }
    }
}

/// Subscribe the trigger side to `order.update` on the critical lane.
pub fn subscribe_order_updates(app: &AppState, side: &Arc<TriggerSide>) {
    app.bus.subscribe(
        Arc::new(FlowOrderUpdates {
            side: Arc::downgrade(side),
        }),
        Lane::Critical,
    );
}
