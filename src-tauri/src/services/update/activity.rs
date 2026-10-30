//! What a restart now would interrupt (Live and sandbox), read from every
//! module that trades, and whether the trader's confirmation covers it.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long reading the broker's order book may take while deciding what a
/// restart interrupts.
const ORDER_BOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// What a restart now would interrupt.
///
/// Sent back by the window as the trader's confirmation: a restart goes
/// ahead only while everything running is within what they confirmed
/// ([`Activity::covered_by`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Activity {
    /// Running Live strategies (the strategy module).
    pub live_strategies: u32,
    /// Running Live OpenScript runs.
    pub live_openscript_runs: u32,
    /// Live positions under scalping protection (stop or target).
    pub live_scalping_stops: u32,
    /// Open orders at the broker (open or trigger pending).
    pub live_open_orders: u32,
    /// The broker's order book could not be read.
    pub live_orders_unknown: bool,
    /// Placements whose outcome is not known yet and exits still filling.
    pub unsettled_orders: u32,
    /// Something running could not be checked.
    pub checks_failed: bool,
    /// Flow workflow runs in progress while analyzer mode is off (or
    /// cannot be read): their orders go to the broker.
    pub live_flow_runs: u32,
    pub sandbox_strategies: u32,
    pub sandbox_openscript_runs: u32,
    pub sandbox_scalping_stops: u32,
    pub sandbox_open_orders: u32,
    /// Flow workflow runs in progress in analyzer mode.
    pub sandbox_flow_runs: u32,
}

impl Activity {
    pub fn has_live(&self) -> bool {
        self.live_strategies
            + self.live_openscript_runs
            + self.live_scalping_stops
            + self.live_open_orders
            + self.unsettled_orders
            + self.live_flow_runs
            > 0
            || self.live_orders_unknown
            || self.checks_failed
    }

    pub fn has_sandbox(&self) -> bool {
        self.sandbox_strategies
            + self.sandbox_openscript_runs
            + self.sandbox_scalping_stops
            + self.sandbox_open_orders
            + self.sandbox_flow_runs
            > 0
    }

    /// Nothing a restart would interrupt.
    pub fn is_quiet(&self) -> bool {
        !self.has_live() && !self.has_sandbox()
    }

    /// Whether everything running now is within what the trader confirmed
    /// (`confirmed`): no count grew and nothing unknown appeared.
    pub fn covered_by(&self, confirmed: &Activity) -> bool {
        let within = |now: u32, ok: u32| now <= ok;
        within(self.live_strategies, confirmed.live_strategies)
            && within(self.live_openscript_runs, confirmed.live_openscript_runs)
            && within(self.live_scalping_stops, confirmed.live_scalping_stops)
            && within(self.live_open_orders, confirmed.live_open_orders)
            && within(self.unsettled_orders, confirmed.unsettled_orders)
            && (!self.live_orders_unknown || confirmed.live_orders_unknown)
            && (!self.checks_failed || confirmed.checks_failed)
            && within(self.sandbox_strategies, confirmed.sandbox_strategies)
            && within(
                self.sandbox_openscript_runs,
                confirmed.sandbox_openscript_runs,
            )
            && within(
                self.sandbox_scalping_stops,
                confirmed.sandbox_scalping_stops,
            )
            && within(self.sandbox_open_orders, confirmed.sandbox_open_orders)
            && within(self.live_flow_runs, confirmed.live_flow_runs)
            && within(self.sandbox_flow_runs, confirmed.sandbox_flow_runs)
    }
}

fn is_open_status(status: &str) -> bool {
    let s = status.trim().to_ascii_lowercase();
    s == "open" || s == "trigger pending" || s == "trigger_pending"
}

/// What a restart now would interrupt, read from every module that trades.
/// Reading the broker's order book is the only network call.
pub async fn activity(ctx: &AppState) -> Activity {
    use crate::strategy::dispatch::RunMode;
    let mut a = Activity::default();
    match ctx.strategy.store.list_open_runs() {
        Ok(runs) => {
            for r in runs {
                match RunMode::parse(&r.mode) {
                    Some(RunMode::Sandbox) => a.sandbox_strategies += 1,
                    // A run whose destination cannot be read counts as Live.
                    _ => a.live_strategies += 1,
                }
            }
        }
        Err(e) => {
            tracing::warn!("Update: could not read the running strategies: {}", e);
            a.checks_failed = true;
        }
    }
    // A Flow run's orders go where analyzer mode sends them; one that
    // cannot be read counts as Live.
    let flow_runs = ctx.flow.running() as u32;
    if flow_runs > 0 {
        match crate::services::core::analyze_mode(ctx) {
            Ok(true) => a.sandbox_flow_runs = flow_runs,
            _ => a.live_flow_runs = flow_runs,
        }
    }
    for run in ctx.trading.runner.running() {
        match run.mode {
            RunMode::Live => a.live_openscript_runs += 1,
            RunMode::Sandbox => a.sandbox_openscript_runs += 1,
        }
    }
    let store = &ctx.scalping.store;
    match (
        store.active_sl(Some(crate::scalping::store::MODE_LIVE)),
        store.active_sl(Some(crate::scalping::store::MODE_ANALYZE)),
    ) {
        (Ok(live), Ok(sandbox)) => {
            a.live_scalping_stops = live.len() as u32;
            a.sandbox_scalping_stops = sandbox.len() as u32;
        }
        (l, s) => {
            if let Err(e) = l.and(s) {
                tracing::warn!("Update: could not read scalping protection: {}", e);
            }
            a.checks_failed = true;
        }
    }
    match ctx.sandbox.orderbook().await {
        Ok(book) => {
            a.sandbox_open_orders = book
                .data
                .orders
                .iter()
                .filter(|o| is_open_status(&o.order_status))
                .count() as u32;
        }
        Err(e) => {
            tracing::warn!("Update: could not read the sandbox order book: {}", e);
            a.checks_failed = true;
        }
    }
    // Live orders at the broker, whatever the analyzer toggle says: a Live
    // order rests at the broker either way.
    if let Ok(h) = crate::services::core::broker_handle(ctx) {
        match tokio::time::timeout(ORDER_BOOK_TIMEOUT, h.broker.get_order_book(&h.auth)).await {
            Ok(Ok(orders)) => {
                a.live_open_orders =
                    orders.iter().filter(|o| is_open_status(&o.status)).count() as u32;
            }
            Ok(Err(e)) => {
                tracing::warn!("Update: could not read the broker's order book: {}", e);
                a.live_orders_unknown = true;
            }
            Err(_) => {
                tracing::warn!("Update: the broker's order book did not answer in time");
                a.live_orders_unknown = true;
            }
        }
        let guard = ctx.switch_guard.read().clone();
        match guard.blockers(ctx, &h.id) {
            Ok(b) => a.unsettled_orders = b.uncertain_placements + b.partial_exits,
            Err(e) => {
                tracing::warn!("Update: could not check unsettled orders: {}", e);
                a.checks_failed = true;
            }
        }
    }
    a
}

/// The logged-in broker's display name, for the warning.
pub(crate) fn broker_name(ctx: &AppState) -> Option<String> {
    ctx.get_broker_session()
        .map(|s| crate::brokers::setup_guide::display_name(&s.broker_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_confirmation_round_trips_through_json() {
        let a = Activity {
            live_scalping_stops: 1,
            live_orders_unknown: true,
            ..Default::default()
        };
        let back: Activity = serde_json::from_value(serde_json::to_value(a).unwrap()).unwrap();
        assert_eq!(back, a);
        // Missing fields read as zero.
        let partial: Activity =
            serde_json::from_value(serde_json::json!({"live_strategies": 2})).unwrap();
        assert_eq!(partial.live_strategies, 2);
        assert!(!partial.checks_failed);
    }

    /// A Flow run in progress is something a restart interrupts, and a
    /// confirmation given before it started does not cover it.
    #[tokio::test]
    async fn a_flow_run_in_progress_is_counted() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        let quiet = activity(&t.ctx).await;
        assert_eq!(quiet.live_flow_runs, 0);
        t.ctx.flow.spawn(std::future::pending());
        let a = activity(&t.ctx).await;
        assert_eq!((a.live_flow_runs, a.sandbox_flow_runs), (1, 0));
        assert!(a.has_live());
        assert!(!a.covered_by(&quiet));
        t.ctx
            .flow
            .stop_all(std::time::Duration::from_millis(10))
            .await;
    }
}
