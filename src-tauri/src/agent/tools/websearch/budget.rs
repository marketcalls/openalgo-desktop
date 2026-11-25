//! The caps on one run's web calls, shared by its tools (the web's
//! `_Budget`): a per-turn budget held in memory for the run, and the daily
//! cap counted in the database so a new message does not reset it.
//! Searches, research and page fetches share both: each is one paid call.
//!
//! The turn counts every request that went out, failures included, so a
//! loop on a failing page or provider is bounded (web a2ae9f1fa); the day
//! counts answers only, so a provider's outage cannot use up the trader's
//! day. A request the gate never sent counts nowhere.

use crate::agent::websearch::payloads::NotOk;
use crate::agent::websearch::settings::{self, Config};
use crate::agent::websearch::today;
use crate::state::AppState;
use parking_lot::Mutex;
use std::sync::Arc;

pub(super) struct Budget {
    /// Read on the first call (the settings rows), then kept for the run.
    config: tokio::sync::OnceCell<Config>,
    used: Mutex<i64>,
}

impl Budget {
    pub(super) fn new() -> Budget {
        Budget {
            config: tokio::sync::OnceCell::new(),
            used: Mutex::new(0),
        }
    }

    /// The run's web search settings, read once.
    pub(super) async fn config(&self, app: &Arc<AppState>) -> Config {
        self.config
            .get_or_init(|| async {
                app.blocking(|c| Ok(Config::read(&*c.sqlite.conn()?)))
                    .await
                    .unwrap_or_else(|_| Config::from_rows(&Default::default()))
            })
            .await
            .clone()
    }

    /// The requests this run has sent so far.
    pub(super) fn used(&self) -> i64 {
        *self.used.lock()
    }

    /// Both caps, checked before a call (`_Budget.refusal`).
    pub(super) async fn refusal(&self, app: &Arc<AppState>) -> Option<NotOk<'static>> {
        let c = self.config(app).await;
        if self.used() >= c.max_calls_per_turn {
            return Some(NotOk::new("per_turn_budget_exhausted", format!(
                "This turn has already used its {} web calls. Answer from what you have and tell the operator you stopped looking.",
                c.max_calls_per_turn)));
        }
        let day = today(app.now());
        let used = app
            .blocking(move |x| Ok(settings::usage(&*x.sqlite.conn()?, &day)))
            .await
            .unwrap_or(0);
        (used >= c.daily_cap).then(|| NotOk::new("daily_cap_reached", format!(
            "The daily web cap of {} calls is reached. Tell the operator; they can raise it in the agent settings.",
            c.daily_cap)))
    }

    /// Count one answered call against both caps (`_Budget.record`).
    pub(super) async fn record(&self, app: &Arc<AppState>) {
        *self.used.lock() += 1;
        let day = today(app.now());
        if let Err(e) = app
            .blocking(move |x| settings::count_one(&*x.sqlite.conn()?, &day))
            .await
        {
            tracing::error!("Could not count a web call against today's cap: {}", e);
        }
    }

    /// Count one request that went out and failed, against this turn only
    /// (`_Budget.record_attempt`).
    pub(super) fn record_attempt(&self) {
        *self.used.lock() += 1;
    }
}
