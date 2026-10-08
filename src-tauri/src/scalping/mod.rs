//! The scalping terminal backend (web `blueprints/scalping.py`,
//! `database/scalping_db.py`, `services/scalping_risk_monitor_service.py`).
//!
//! | File | Concern |
//! |---|---|
//! | `store` | `scalping_sl_state` and `scalping_tracked_symbol` |
//! | `monitor` | the tick-driven stop / target / trailing monitor |
//! | `service` | what the `/scalping/api` routes do (validation, orders, exits) |
//!
//! Departures from the web, deliberate:
//!
//! * The monitor watches the stops of **both** modes and exits each leg in
//!   the mode it was opened in (`force_live` for a live leg, the sandbox for
//!   a sandbox leg). The web watches only the current mode and, when the
//!   analyzer toggle changed between a breach and its exit, skips the exit;
//!   the desktop keeps the leg protected instead.
//! * A position book that cannot be read refuses the exit (the leg stays
//!   managed); the web read it as flat and cleared the stop.
//! * Monitor exits never wait in the Action Center.
//!
//! Lifecycle: the monitor starts when a broker session starts and stops
//! (task aborted, subscriptions released, legs forgotten) when it ends, by a
//! bus subscriber, and on app shutdown.

pub mod monitor;
pub mod service;
pub mod store;

use crate::events::{Event, EventBus, Lane, Subscriber, Topic};
use std::sync::{Arc, Weak};

pub use monitor::{MonitorDeps, RiskMonitor};
pub use store::Store;

/// One scalping backend per app context.
pub struct Scalping {
    pub store: Store,
    pub monitor: Arc<RiskMonitor>,
}

impl Scalping {
    pub fn new(deps: MonitorDeps) -> Arc<Self> {
        let store = deps.store.clone();
        Arc::new(Self {
            store,
            monitor: RiskMonitor::new(deps),
        })
    }

    pub async fn shutdown(&self) {
        self.monitor.stop().await;
    }
}

/// Starts the monitor with a broker session and stops it when the session
/// ends (logout, the daily boundary).
struct Lifecycle {
    monitor: Weak<RiskMonitor>,
}

#[async_trait::async_trait]
impl Subscriber for Lifecycle {
    fn name(&self) -> &'static str {
        "scalping-monitor"
    }

    fn topics(&self) -> Vec<Topic> {
        vec![Topic::BrokerConnected, Topic::BrokerSessionEnded]
    }

    async fn handle(&self, event: Arc<Event>) {
        let Some(m) = self.monitor.upgrade() else {
            return;
        };
        match event.as_ref() {
            Event::BrokerConnected { .. } => m.start(),
            Event::BrokerSessionEnded { .. } => m.stop().await,
            _ => {}
        }
    }
}

/// Subscribe the monitor's lifecycle on the bus.
pub fn register(bus: &EventBus, scalping: &Arc<Scalping>) {
    bus.subscribe(
        Arc::new(Lifecycle {
            monitor: Arc::downgrade(&scalping.monitor),
        }),
        Lane::Critical,
    );
}
