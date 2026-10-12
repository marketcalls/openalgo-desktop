//! The strategy module and RMS (web `services/strategy_module/`): multi-leg
//! strategies with end-to-end risk management, and the signal-driven mode
//! that reacts to individual TradingView alerts.
//!
//! Two kinds share one engine: `batch` enters and exits every leg together
//! (`start` / `stop`), `signal` moves one leg per alert (`long_entry`,
//! `long_exit`, `short_entry`, `short_exit`).
//!
//! The rules are not here: `risk_adapter` translates run state into the pure
//! `crate::risk` core and back. The invariants that reversed real positions
//! on the web are held here exactly:
//!
//! * **Claim under the same lock that checks.** `state::claim_leg_exit` and
//!   friends write the `exit_kind` marker in the same hold as the duplicate
//!   check, before dispatch; a refused dispatch releases exactly that claim.
//! * **A fill is matched to the order it belongs to**, by order row and
//!   position reference, never by leg alone.
//! * **A live run passes `force_live`**; a sandbox run calls the sandbox
//!   directly. The global analyzer toggle never decides a run's destination.
//! * **A stop whose exits were refused leaves the run open and managed.**
//!
//! | File | Concern |
//! |---|---|
//! | `store` | the six `sm_` tables |
//! | `state` | live run state and every claim |
//! | `risk_adapter` | leg and run state to the risk core and back |
//! | `dispatch` | order building and the run's pipe (live or sandbox) |
//! | `engine` | start, stop, close leg, fills, ticks, finalise |
//! | `signals` | signal-mode protocol |
//! | `order_events` | `order.update` folded into order rows and fills |
//! | `broadcast` | the six `strategy_*` Socket.IO frames |
//! | `webhook` | the public `/strategy/webhook/<token>` pipeline |
//! | `scheduler` | IST start and square-off jobs, pending-stop retries |
//! | `checkpoint` | periodic snapshots |
//! | `recovery` | rebuild open runs at startup |
//! | `tick_feed` | refcounted price subscriptions driving `process_tick` |
//! | `resolver` | leg to exact contract |
//! | `validate` | configuration validation (the session API's rules) |
//! | `views` | broker-backed books per strategy |
//! | `book` | the per-strategy position book (`strategy_order_tags`) |

pub mod book;
pub mod broadcast;
pub mod checkpoint;
pub mod dispatch;
pub mod engine;
pub mod order_events;
pub mod reconcile;
pub mod recovery;
pub mod resolver;
pub mod risk_adapter;
pub mod scheduler;
pub mod session;
pub mod signals;
pub mod state;
pub mod store;
pub mod tick_feed;
pub mod validate;
pub mod views;
pub mod webhook;

use crate::brokers::common::symbols::SymbolResolver;
use crate::clock::Clock;
use crate::db::sqlite::SqliteDb;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub use dispatch::{OrderGateway, RunMode};
pub use engine::StartResult;
pub use store::Store;

/// Everything the module needs from the app, injectable for tests.
pub struct Deps {
    pub db: Arc<SqliteDb>,
    pub gateway: Arc<dyn OrderGateway>,
    pub rooms: Arc<dyn broadcast::RoomEmitter>,
    pub clock: Arc<dyn Clock>,
    pub symbols: SymbolResolver,
    /// The platform session boundary (`SESSION_EXPIRY_TIME`, IST).
    pub session_hour: u32,
    pub session_minute: u32,
    /// Price subscriptions for runs (the market-data feed in production).
    pub prices: Option<Arc<dyn tick_feed::PriceSource>>,
}

/// Subscribe the module's bus consumers: order updates for strategy runs,
/// and the per-strategy position book.
pub fn register(ctx: &Arc<crate::state::AppState>) {
    order_events::register(&ctx.bus, &ctx.strategy);
    if let Err(e) = ctx.strategy.book.prune() {
        tracing::warn!("Could not prune the strategy book: {}", e);
    }
    book::register(&ctx.bus, ctx.strategy.book.clone());
}

/// One strategy module per app context.
pub struct StrategyModule {
    pub store: Store,
    pub state: state::StateRegistry,
    pub gateway: Arc<dyn OrderGateway>,
    pub broadcast: broadcast::Broadcaster,
    pub clock: Arc<dyn Clock>,
    pub symbols: SymbolResolver,
    pub webhook: webhook::WebhookState,
    pub order_events: order_events::Stash,
    pub feed: tick_feed::TickFeed,
    pub scheduler: scheduler::Scheduler,
    /// The per-strategy position book fed by `order.placed` tags.
    pub book: Arc<book::StrategyBook>,
    pub(crate) session: (u32, u32),
    /// Runs whose risk fired while no broker session existed (one critical
    /// event per episode). Bounded by the number of open runs.
    unactionable: Mutex<HashSet<i64>>,
    /// One async lock per strategy while its signal day-run is found or
    /// opened; entries removed when unheld.
    day_run_locks: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    tasks: Mutex<JoinSet<()>>,
    shutdown: CancellationToken,
    /// Reads that missed each unconfirmed order row (pruned every pass).
    pub(crate) reconcile_absence: crate::services::order_reconciler::AbsenceTracker,
    /// Unconfirmed rows already reported as ambiguous (pruned every pass).
    pub(crate) reconcile_ambiguous: Mutex<HashSet<String>>,
    /// What decides whether new entries may be placed now (the order
    /// reconciler while a gap in order facts is open).
    entry_gate: std::sync::OnceLock<Arc<dyn crate::services::order_reconciler::EntryGate>>,
}

impl StrategyModule {
    pub fn new(deps: Deps) -> Arc<Self> {
        let store = Store::new(deps.db.clone(), deps.clock.clone());
        let book = Arc::new(book::StrategyBook::new(
            deps.db,
            deps.clock.clone(),
            (deps.session_hour, deps.session_minute),
        ));
        Arc::new(Self {
            store,
            state: state::StateRegistry::new(),
            gateway: deps.gateway,
            broadcast: broadcast::Broadcaster::new(deps.rooms, deps.clock.clone()),
            clock: deps.clock.clone(),
            symbols: deps.symbols,
            webhook: webhook::WebhookState::new(),
            order_events: order_events::Stash::new(),
            feed: tick_feed::TickFeed::new(deps.prices),
            scheduler: scheduler::Scheduler::new(),
            book,
            session: (deps.session_hour, deps.session_minute),
            unactionable: Mutex::new(HashSet::new()),
            day_run_locks: Mutex::new(HashMap::new()),
            tasks: Mutex::new(JoinSet::new()),
            shutdown: CancellationToken::new(),
            reconcile_absence: crate::services::order_reconciler::AbsenceTracker::new(),
            reconcile_ambiguous: Mutex::new(HashSet::new()),
            entry_gate: std::sync::OnceLock::new(),
        })
    }

    /// Gate new entries on `gate` (set once, by the app).
    pub fn set_entry_gate(&self, gate: Arc<dyn crate::services::order_reconciler::EntryGate>) {
        let _ = self.entry_gate.set(gate);
    }

    /// Why a new entry in `mode` must wait right now, if it must.
    pub fn entry_refusal(&self, mode: RunMode) -> Option<String> {
        self.entry_gate.get().and_then(|g| g.entry_refusal(mode))
    }

    /// Record one audit event and push the stored row. Never fails the caller:
    /// an engine that fell over on an audit write would turn a bookkeeping
    /// problem into an open position.
    pub async fn emit(
        &self,
        strategy_id: i64,
        user_id: &str,
        kind: &str,
        message: &str,
        fields: store::EventFields,
    ) {
        match self
            .store
            .record_event(strategy_id, user_id, kind, message, &fields)
        {
            Ok(row) => {
                self.broadcast.push_event(strategy_id, row.to_dict()).await;
            }
            Err(e) => tracing::error!(
                "Could not record strategy event {} for strategy {}: {}",
                kind,
                strategy_id,
                e
            ),
        }
    }

    pub(crate) fn claim_unactionable(&self, run_id: i64) -> bool {
        self.unactionable.lock().insert(run_id)
    }

    pub(crate) fn release_unactionable(&self, run_id: i64) -> bool {
        self.unactionable.lock().remove(&run_id)
    }

    pub(crate) fn day_run_lock(&self, strategy_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.day_run_locks.lock();
        // Drop locks nobody holds (bounded by strategies being opened now).
        map.retain(|_, l| Arc::strong_count(l) > 1);
        map.entry(strategy_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Background work in flight (hygiene tests).
    pub fn task_count(&self) -> usize {
        let mut t = self.tasks.lock();
        while t.try_join_next().is_some() {}
        t.len()
    }

    pub(crate) fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(fut);
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Bring the module up in the order the pieces need (web `runtime.py`):
    /// recovery before any price, the price hook before subscriptions,
    /// checkpointing, then the scheduler last. Each step is guarded.
    pub async fn start(self: &Arc<Self>) -> Value {
        let recovered = match recovery::recover_all(self).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Strategy recovery failed: {}", e);
                HashMap::new()
            }
        };
        for (run_id, symbols) in &recovered {
            self.feed.add_run(*run_id, symbols).await;
        }
        tick_feed::start(self);
        checkpoint::start(self);
        scheduler::start(self);
        serde_json::json!({
            "recovery": recovered.len(),
            "tick_feed": true,
            "checkpoint": true,
            "scheduler": true,
        })
    }

    /// Stop every background task this module owns and release its price
    /// subscriptions.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        self.feed.release_all().await;
    }
}
