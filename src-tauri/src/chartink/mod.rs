//! Chartink strategies (web `blueprints/chartink.py`,
//! `database/chartink_db.py`): scanner alerts turned into orders.
//!
//! | File | Concern |
//! |---|---|
//! | `store` | `chartink_strategies`, `chartink_symbol_mappings` |
//! | `webhook` | the public webhook's guard and the alert's order semantics |
//! | `service` | what the `/chartink` routes do |
//!
//! Orders from an alert are queued like the web's two queues and placed by
//! one owned worker through the `/api/v1` services (`Route::API`: the web
//! posts them to its own API with the API key, so analyzer mode and
//! Semi-Auto apply exactly as they do for any API client). Regular orders
//! go at most ten per second; each smart order is followed by a one second
//! pause, and smart orders are taken first.
//!
//! An intraday strategy is squared off at its square-off time (IST) every
//! day it is active, by one owned scheduler task that checks each minute
//! (the web's cron job, with its five minute misfire grace).

pub mod service;
pub mod store;
pub mod webhook;

use crate::clock::Clock;
use crate::services::order_service::Route;
use crate::state::AppState;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub use store::Store;

/// Orders waiting per queue; beyond it an alert is refused, not buffered.
pub const QUEUE_CAP: usize = 1000;
/// Regular orders per second (web: 10).
pub const REGULAR_PER_SECOND: usize = 10;
/// Pause after each smart order (web: 1 second).
pub const SMART_PAUSE: Duration = Duration::from_secs(1);
/// How late a square-off may still run (web misfire grace).
pub const SQUAREOFF_GRACE_MINUTES: i64 = 5;
/// How often the square-off scheduler looks at the clock.
pub const SQUAREOFF_TICK: Duration = Duration::from_secs(20);

/// One queued order: smart (`placesmartorder`) or regular (`placeorder`).
#[derive(Debug, Clone)]
pub struct Queued {
    pub smart: bool,
    pub request: Value,
}

struct Queues {
    regular: mpsc::Sender<Queued>,
    smart: mpsc::Sender<Queued>,
}

pub struct Chartink {
    pub store: Store,
    pub guard: webhook::Guard,
    ctx: Weak<AppState>,
    clock: Arc<dyn Clock>,
    queues: Mutex<Option<Queues>>,
    /// Square-offs done, by strategy, with the IST day (bounded by strategies).
    squared: Mutex<HashMap<i64, NaiveDate>>,
    tasks: Mutex<JoinSet<()>>,
    shutdown: CancellationToken,
    scheduler_started: Mutex<bool>,
}

impl Chartink {
    pub fn new(
        ctx: Weak<AppState>,
        db: Arc<crate::db::sqlite::SqliteDb>,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store: Store::new(db),
            guard: webhook::Guard::new(),
            ctx,
            clock,
            queues: Mutex::new(None),
            squared: Mutex::new(HashMap::new()),
            tasks: Mutex::new(JoinSet::new()),
            shutdown: CancellationToken::new(),
            scheduler_started: Mutex::new(false),
        })
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(fut);
    }

    /// Owned tasks alive (worker and scheduler).
    pub fn task_count(&self) -> usize {
        let mut t = self.tasks.lock();
        while t.try_join_next().is_some() {}
        t.len()
    }

    /// Start the square-off scheduler (idempotent).
    pub fn start(self: &Arc<Self>) {
        {
            let mut started = self.scheduler_started.lock();
            if *started || self.shutdown.is_cancelled() {
                return;
            }
            *started = true;
        }
        let weak = Arc::downgrade(self);
        let token = self.shutdown.clone();
        self.spawn(async move {
            let mut tick = tokio::time::interval(SQUAREOFF_TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tick.tick() => {
                        let Some(m) = weak.upgrade() else { break };
                        let now = m.now();
                        m.run_due_squareoffs(now);
                    }
                }
            }
        });
    }

    /// Stop the worker and the scheduler; queued orders are dropped.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.queues.lock().take();
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }

    // ------------------------------------------------------------ order queue

    fn ensure_worker(self: &Arc<Self>) -> Option<(mpsc::Sender<Queued>, mpsc::Sender<Queued>)> {
        if self.shutdown.is_cancelled() {
            return None;
        }
        let mut q = self.queues.lock();
        if let Some(q) = q.as_ref() {
            if !q.regular.is_closed() {
                return Some((q.regular.clone(), q.smart.clone()));
            }
        }
        let (rtx, rrx) = mpsc::channel(QUEUE_CAP);
        let (stx, srx) = mpsc::channel(QUEUE_CAP);
        *q = Some(Queues {
            regular: rtx.clone(),
            smart: stx.clone(),
        });
        drop(q);
        let weak = Arc::downgrade(self);
        let token = self.shutdown.clone();
        self.spawn(worker(weak, token, rrx, srx));
        Some((rtx, stx))
    }

    /// Queue orders for placement. False when the queue is full or the app
    /// is stopping (nothing of this batch beyond that point is queued).
    pub fn enqueue(self: &Arc<Self>, orders: Vec<Queued>) -> bool {
        let Some((regular, smart)) = self.ensure_worker() else {
            return false;
        };
        for o in orders {
            let tx = if o.smart { &smart } else { &regular };
            if tx.try_send(o).is_err() {
                tracing::error!(
                    "Chartink order queue is full; the rest of this alert was not queued"
                );
                return false;
            }
        }
        true
    }

    async fn place(&self, q: &Queued) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        let symbol = q
            .request
            .get("symbol")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let strategy = q
            .request
            .get("strategy")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let reply = if q.smart {
            crate::services::order_service::place_smart_order(&ctx, &q.request, Route::API).await
        } else {
            crate::services::order_service::place_order(&ctx, &q.request, Route::API).await
        };
        let kind = if q.smart { "Smart order" } else { "Order" };
        if reply.is_success() && reply.body.get("status").and_then(Value::as_str) != Some("error") {
            tracing::info!("{} placed for {} in strategy {}", kind, symbol, strategy);
        } else {
            tracing::error!(
                "{} for {} in strategy {} was not placed: {}",
                kind,
                symbol,
                strategy,
                reply.message()
            );
        }
    }

    // ------------------------------------------------------------ square-off

    /// Square off every active intraday strategy whose time has come today
    /// (within the grace), once per IST day. Returns the strategies done.
    pub fn run_due_squareoffs(self: &Arc<Self>, now: DateTime<Utc>) -> Vec<i64> {
        let ist = now.with_timezone(&Kolkata);
        let today = ist.date_naive();
        let t = ist.time();
        let strategies = match self.store.all() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    "Chartink strategies could not be read for square-off: {}",
                    e
                );
                return vec![];
            }
        };
        let mut done = Vec::new();
        {
            let mut squared = self.squared.lock();
            squared.retain(|id, _| strategies.iter().any(|s| s.id == *id));
        }
        for s in strategies {
            if !s.is_active || !s.is_intraday {
                continue;
            }
            let Some(at) = s
                .squareoff_time
                .as_deref()
                .and_then(|x| NaiveTime::parse_from_str(x.trim(), "%H:%M").ok())
            else {
                continue;
            };
            let late = (t - at).num_minutes();
            if t < at || late > SQUAREOFF_GRACE_MINUTES {
                continue;
            }
            {
                let mut squared = self.squared.lock();
                if squared.get(&s.id) == Some(&today) {
                    continue;
                }
                squared.insert(s.id, today);
            }
            let mappings = self.store.mappings(s.id).unwrap_or_default();
            let orders: Vec<Queued> = mappings
                .iter()
                .map(|m| Queued {
                    smart: true,
                    request: json!({
                        "strategy": s.name,
                        "symbol": m.chartink_symbol,
                        "exchange": m.exchange,
                        "action": "SELL",
                        "product": m.product_type,
                        "pricetype": "MARKET",
                        "quantity": 0,
                        "position_size": 0,
                        "price": 0,
                        "trigger_price": 0,
                        "disclosed_quantity": 0,
                    }),
                })
                .collect();
            tracing::info!(
                "Squaring off Chartink strategy {} ({} symbols)",
                s.id,
                orders.len()
            );
            self.enqueue(orders);
            done.push(s.id);
        }
        done
    }
}

/// The order worker: smart orders first (one second apart), then regular
/// orders at most ten per second.
async fn worker(
    module: Weak<Chartink>,
    token: CancellationToken,
    mut regular: mpsc::Receiver<Queued>,
    mut smart: mpsc::Receiver<Queued>,
) {
    let mut recent: VecDeque<tokio::time::Instant> = VecDeque::with_capacity(REGULAR_PER_SECOND);
    loop {
        tokio::select! {
            biased;
            _ = token.cancelled() => break,
            Some(o) = smart.recv() => {
                let Some(m) = module.upgrade() else { break };
                m.place(&o).await;
                drop(m);
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tokio::time::sleep(SMART_PAUSE) => {}
                }
            }
            Some(o) = regular.recv() => {
                let now = tokio::time::Instant::now();
                while recent.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(1)) {
                    recent.pop_front();
                }
                if recent.len() >= REGULAR_PER_SECOND {
                    if let Some(first) = recent.front().copied() {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            _ = tokio::time::sleep_until(first + Duration::from_secs(1)) => {}
                        }
                    }
                    recent.pop_front();
                }
                let Some(m) = module.upgrade() else { break };
                m.place(&o).await;
                recent.push_back(tokio::time::Instant::now());
            }
            else => break,
        }
    }
}

/// The alert pipeline after the guard (web order of checks and bodies).
/// Returns the HTTP status and body.
pub async fn handle_alert(
    module: &Arc<Chartink>,
    strategy: &store::Strategy,
    body: &[u8],
) -> (u16, Value) {
    let err = |status: u16, msg: &str| (status, json!({"status": "error", "error": msg}));
    if !strategy.is_active {
        tracing::info!(
            "Chartink strategy {} is inactive; alert ignored",
            strategy.id
        );
        return (
            200,
            json!({"status": "success", "message": "Strategy is inactive"}),
        );
    }
    let data = match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) if !m.is_empty() => m,
        _ => {
            tracing::error!(
                "Chartink alert for strategy {} carried no data",
                strategy.id
            );
            return err(400, "No data received");
        }
    };
    let text = |k: &str| match data.get(k) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let Some(signal) = webhook::signal_for(&text("scan_name")) else {
        return err(
            400,
            "No valid action keyword (BUY/SELL/SHORT/COVER) found in scan name",
        );
    };
    if let Some(msg) = webhook::window_refusal(strategy, signal, module.now()) {
        tracing::info!("Chartink strategy {}: {}", strategy.id, msg);
        return err(400, msg);
    }
    let mappings = match module.store.mappings(strategy.id) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(
                "Chartink mappings for strategy {} could not be read: {}",
                strategy.id,
                e
            );
            Vec::new()
        }
    };
    if mappings.is_empty() {
        return err(400, "No symbol mappings configured");
    }
    let (orders, processed) = webhook::orders_for(strategy, signal, &text("stocks"), &mappings);
    if processed.is_empty() {
        return (
            200,
            json!({"status": "warning", "message": "No orders were queued"}),
        );
    }
    let queued: Vec<Queued> = orders
        .into_iter()
        .map(|(smart, request)| Queued { smart, request })
        .collect();
    if !module.enqueue(queued) {
        return err(
            503,
            "Orders could not be queued right now. Try the alert again in a moment.",
        );
    }
    (
        200,
        json!({
            "status": "success",
            "message": format!("Orders queued for symbols: {}", processed.join(", ")),
        }),
    )
}
