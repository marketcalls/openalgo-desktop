//! Flow (`/flow`): visual workflows that trade, at parity with OpenAlgo
//! web's Flow (`blueprints/flow.py`, `services/flow_*.py`,
//! `database/flow_db.py`). Native Rust; a workflow exported from the web
//! imports here unchanged, and back.
//!
//! What lives where:
//!
//! - `contract/`: the interfaces every part codes against: the stored model,
//!   the node trait and its context, interpolation, the client nodes reach
//!   OpenAlgo through, the subscription record and the trigger hooks.
//! - `executor/`: running a workflow: the one-run-per-workflow claim, the
//!   owned run task, the record, the graph walk.
//! - `indicator_service`: the port of `services/indicator_service.py` the
//!   Indicator, Bar Offset, Prior Period OHLC and History nodes compute with.
//! - `node_contracts`: statuses, expiry and strike vocabularies, default
//!   product, expiry selection (the web's `flow_node_contracts.py`).
//! - `nodes/`: the node registry, one line per node family.
//! - `service/`: what the `/flow/api/...` routes do.
//! - `store/`: the workflows and their run history (migration
//!   `100_flow_core`).
//! - `validator/`: the web's workflow validator.
//!
//! Every run is a task this module owns; [`FlowModule::stop_all`] ends them
//! at sign-out, lock, reset and quit, and a stopped run writes its record
//! before it ends.

pub mod contract;
pub mod executor;
pub mod indicator_service;
pub mod node_contracts;
pub mod nodes;
pub mod service;
pub mod store;
pub mod validator;

#[cfg(test)]
mod goldens;

use contract::env::NodeEnv;
use contract::subscriptions::Subscriptions;
use contract::triggers::{NoTriggers, TriggerHooks};
use nodes::NodeRegistry;
use parking_lot::{Mutex, RwLock};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// At app start: close the runs a quit or crash left `running`, so the
/// history never shows a run as going that nothing runs (lead decision 6).
pub fn start(ctx: &Arc<crate::state::AppState>) {
    let closed = ctx
        .sqlite
        .conn()
        .and_then(|c| store::executions::close_interrupted(&c, ctx.now()));
    match closed {
        Ok(0) => {}
        Ok(n) => tracing::info!("Closed {} Flow run(s) the last session left unfinished", n),
        Err(e) => tracing::warn!("Could not close the Flow runs the last session left: {}", e),
    }
}

/// How long a stopped run has to write its record before it is aborted.
pub const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Flow's state, owned by the app context.
pub struct FlowModule {
    /// Every node type's behaviour.
    pub nodes: NodeRegistry,
    /// The feed subscriptions each workflow holds.
    pub subscriptions: Arc<Subscriptions>,
    /// The scheduler and monitors' side ([`NoTriggers`] until installed).
    triggers: RwLock<Arc<dyn TriggerHooks>>,
    /// What nodes read besides the client, as the node lanes install it.
    env: RwLock<NodeEnv>,
    /// The workflows claimed now (a run, a delete or a deactivate).
    claimed: Arc<Mutex<HashSet<i64>>>,
    /// The runs in flight.
    tasks: Mutex<JoinSet<()>>,
    /// Fires for every run in flight when the app stops them; replaced
    /// afterwards so runs after the next sign-in start clean.
    stop: Mutex<CancellationToken>,
}

impl Default for FlowModule {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowModule {
    pub fn new() -> Self {
        Self {
            nodes: NodeRegistry::default(),
            subscriptions: Arc::new(Subscriptions::default()),
            triggers: RwLock::new(Arc::new(NoTriggers)),
            env: RwLock::new(NodeEnv::default()),
            claimed: Arc::default(),
            tasks: Mutex::new(JoinSet::new()),
            stop: Mutex::new(CancellationToken::new()),
        }
    }

    /// Install the trigger side's hooks (the scheduler and monitors).
    pub fn set_triggers(&self, hooks: Arc<dyn TriggerHooks>) {
        *self.triggers.write() = hooks;
    }

    /// The trigger side's hooks.
    pub fn triggers(&self) -> Arc<dyn TriggerHooks> {
        self.triggers.read().clone()
    }

    /// Install a node lane's real services (the symbol master, the feed,
    /// ...) for every later run.
    pub fn install_env(&self, f: impl FnOnce(&mut NodeEnv)) {
        f(&mut self.env.write());
    }

    /// The services a new run's nodes read.
    pub fn env(&self) -> NodeEnv {
        self.env.read().clone()
    }

    /// Claim a workflow for a run, a delete or a deactivate; `None` while
    /// another holds it (the web's per-workflow try-lock, never waited on).
    /// The claim ends when the guard drops, on every path.
    pub fn try_claim(&self, workflow_id: i64) -> Option<Claim> {
        if !self.claimed.lock().insert(workflow_id) {
            return None;
        }
        Some(Claim {
            workflow_id,
            claimed: self.claimed.clone(),
        })
    }

    /// The token a new run stops on.
    pub fn stop_token(&self) -> CancellationToken {
        self.stop.lock().child_token()
    }

    /// Run `fut` as a task this module owns (a run, so a dropped request
    /// never cuts an order off mid-flight).
    pub fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(fut);
    }

    /// Runs in flight.
    pub fn running(&self) -> usize {
        let mut tasks = self.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.len()
    }

    /// Stop every run: fire their tokens, give them `grace` to write their
    /// records, then abort what is left (sign-out, lock, reset, quit).
    pub async fn stop_all(&self, grace: std::time::Duration) {
        let old = std::mem::replace(&mut *self.stop.lock(), CancellationToken::new());
        old.cancel();
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        let drained =
            tokio::time::timeout(grace, async { while tasks.join_next().await.is_some() {} }).await;
        if drained.is_err() {
            tracing::warn!("Flow runs did not stop in time; ending them");
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
    }
}

/// A workflow held by one run, delete or deactivate.
pub struct Claim {
    workflow_id: i64,
    claimed: Arc<Mutex<HashSet<i64>>>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.claimed.lock().remove(&self.workflow_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A lane's installed service reaches every later run's context.
    #[test]
    fn installed_services_reach_new_runs() {
        struct Book;
        impl contract::env::StrategyBook for Book {
            fn legs(&self, strategy: &str) -> Result<Vec<serde_json::Value>, String> {
                Ok(vec![serde_json::json!({"strategy": strategy})])
            }
        }
        let flow = FlowModule::new();
        assert!(flow.env().strategy_book.legs("s").is_err());
        flow.install_env(|e| e.strategy_book = Arc::new(Book));
        assert_eq!(
            flow.env().strategy_book.legs("s").unwrap()[0]["strategy"],
            "s"
        );
    }

    #[test]
    fn a_workflow_is_claimed_once_until_released() {
        let flow = FlowModule::new();
        let first = flow.try_claim(7);
        assert!(first.is_some());
        assert!(flow.try_claim(7).is_none());
        assert!(flow.try_claim(8).is_some(), "other workflows are free");
        drop(first);
        assert!(flow.try_claim(7).is_some());
    }

    #[tokio::test]
    async fn stopping_ends_every_run_and_leaves_new_runs_clean() {
        let flow = FlowModule::new();
        let token = flow.stop_token();
        let (tx, rx) = tokio::sync::oneshot::channel();
        flow.spawn(async move {
            token.cancelled().await;
            let _ = tx.send("stopped");
        });
        // A run that ignores its token is aborted after the grace period.
        flow.spawn(std::future::pending());
        assert_eq!(flow.running(), 2);
        flow.stop_all(Duration::from_millis(50)).await;
        assert_eq!(rx.await.ok(), Some("stopped"));
        assert_eq!(flow.running(), 0);
        assert!(!flow.stop_token().is_cancelled());
    }
}
