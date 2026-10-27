//! Broker order updates folded into strategy state (web
//! `services/strategy_module/order_events.py`).
//!
//! One subscriber on the bus topic `order.update` (critical lane). Deciding
//! an update is not ours costs one indexed lookup on `broker_order_id`.
//!
//! * **A fill is applied exactly once**: the order row's cumulative facts are
//!   the guard (`Store::fold_order_broker_frame`), so a fill reported twice
//!   adds its quantity once.
//! * **A rejection is final**: a late working frame cannot reopen it.
//! * **A fill is applied only to the order it belongs to**: the row id and
//!   position reference travel with the fill, so a signal flip's exit fill
//!   settles the outgoing position, never the one just opened.
//! * **A price must be strictly positive and finite** (the risk core's own
//!   `is_price`), and a partial fill resizes the leg to what traded.
//!
//! A sandbox MARKET order fills inside the dispatch call, before its row
//! carries the broker id. Such frames are held in a bounded stash (512 ids,
//! 16 frames each, 120 s) and replayed the moment the row is bound.

use super::engine::{FillOpts, FillOutcome};
use super::store::{frame_changes, OrderFactFold, OrderRow, RunRow, StrategyRow};
use super::StrategyModule;
use crate::events::{Event, Lane, OrderUpdate, Subscriber, Topic};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

pub const MAX_HELD_ORDERS: usize = 512;
pub const MAX_FRAMES_PER_ORDER: usize = 16;
pub const HOLD_TTL: Duration = Duration::from_secs(120);

const FILLED: &[&str] = &["complete", "completed", "filled", "executed", "traded"];
const CANCELLED: &[&str] = &["cancelled", "canceled"];
const DEAD: &[&str] = &["rejected", "cancelled", "canceled"];

#[derive(Default)]
struct StashInner {
    frames: HashMap<String, (Instant, Vec<OrderUpdate>)>,
    replays: u64,
}

/// Frames that arrived before their order row carried the broker id.
#[derive(Default)]
pub struct Stash {
    inner: Mutex<StashInner>,
}

impl Stash {
    pub fn new() -> Self {
        Self::default()
    }

    fn replays(&self) -> u64 {
        self.inner.lock().replays
    }

    /// Hold a frame; says whether a replay ran since `replays_before`.
    fn hold(&self, order_id: &str, frame: OrderUpdate, replays_before: u64) -> bool {
        let mut s = self.inner.lock();
        let now = Instant::now();
        s.frames
            .retain(|_, (at, _)| now.saturating_duration_since(*at) < HOLD_TTL);
        if s.frames.len() >= MAX_HELD_ORDERS && !s.frames.contains_key(order_id) {
            // Evict the oldest: almost always another surface's order that
            // nothing will ever claim.
            if let Some(oldest) = s
                .frames
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            {
                s.frames.remove(&oldest);
            }
        }
        let entry = s
            .frames
            .entry(order_id.to_string())
            .or_insert_with(|| (now, Vec::new()));
        entry.1.push(frame);
        if entry.1.len() > MAX_FRAMES_PER_ORDER {
            let excess = entry.1.len() - MAX_FRAMES_PER_ORDER;
            entry.1.drain(..excess);
        }
        s.replays != replays_before
    }

    fn take(&self, order_id: &str) -> Vec<OrderUpdate> {
        self.inner
            .lock()
            .frames
            .remove(order_id)
            .map(|(_, f)| f)
            .unwrap_or_default()
    }

    fn bump_and_take(&self, order_id: &str) -> Vec<OrderUpdate> {
        let mut s = self.inner.lock();
        s.replays += 1;
        s.frames
            .remove(order_id)
            .map(|(_, f)| f)
            .unwrap_or_default()
    }

    /// Orders with held frames (hygiene tests).
    pub fn len(&self) -> usize {
        self.inner.lock().frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn normalise(status: &str) -> String {
    status.trim().to_ascii_lowercase().replace('_', " ")
}

/// A strictly positive finite price (the same predicate a tick must pass).
fn usable_price(v: Option<f64>) -> Option<f64> {
    v.filter(|p| crate::risk::is_price(Some(*p)))
}

/// The price of only the newly reported cumulative fill delta.
fn incremental_fill_price(fold: &OrderFactFold) -> Option<f64> {
    if fold.fill_delta <= 0 {
        return None;
    }
    let cumulative = usable_price(fold.average_fill_price)?;
    match usable_price(fold.previous_average_fill_price) {
        Some(prev) if fold.previous_filled_qty > 0 => {
            let notional = cumulative * fold.cumulative_filled_qty as f64
                - prev * fold.previous_filled_qty as f64;
            usable_price(Some(notional / fold.fill_delta as f64))
        }
        _ => Some(cumulative),
    }
}

/// A frame's fill as the run applies it: the price of what it newly
/// reports (an entry's average, an exit's increment) and how it names its
/// order and position.
fn fill_of(row: &OrderRow, fold: &OrderFactFold, is_entry: bool) -> (Option<f64>, FillOpts) {
    let price = if is_entry {
        usable_price(fold.average_fill_price)
    } else {
        incremental_fill_price(fold)
    };
    let opts = FillOpts {
        filled_qty: Some(fold.fill_delta),
        order_row_id: Some(row.id),
        position_ref: row.position_ref.clone(),
        cumulative_filled_qty: (is_entry && (!fold.terminal() || fold.previous_filled_qty > 0))
            .then_some(fold.cumulative_filled_qty),
        order_terminal: fold.terminal(),
        allow_prior_order_correction: fold.was_terminal(),
    };
    (price, opts)
}

/// The facts of one broker frame as its order row folds them.
struct FrameFacts {
    status: &'static str,
    filled_qty: Option<i64>,
    price: Option<f64>,
    rejection: Option<String>,
}

impl FrameFacts {
    fn of(u: &OrderUpdate) -> Self {
        Self {
            status: canonical_status(&normalise(&u.order_status)),
            filled_qty: (u.filled_quantity > 0).then_some(u.filled_quantity),
            price: usable_price(Some(u.average_price)),
            rejection: (!u.rejection_reason.is_empty()).then(|| u.rejection_reason.clone()),
        }
    }
}

/// One targeted order-status response, or an order-book row, as an
/// `order.update` frame; a row that names no order id is `broker_order_id`.
fn snapshot_frame(broker_order_id: &str, order: &Value) -> OrderUpdate {
    let get = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| order.get(*k).filter(|v| !v.is_null()))
    };
    let s = |keys: &[&str]| get(keys).and_then(Value::as_str).unwrap_or("").to_string();
    let n = |keys: &[&str]| get(keys).and_then(crate::risk::value_to_f64).unwrap_or(0.0);
    let id = {
        let v = s(&["orderid", "order_id"]);
        if v.is_empty() {
            broker_order_id.to_string()
        } else {
            v
        }
    };
    OrderUpdate {
        orderid: id,
        symbol: s(&["symbol"]),
        exchange: s(&["exchange"]),
        action: s(&["action"]),
        quantity: n(&["quantity", "qty"]) as i64,
        order_status: s(&["order_status", "orderstatus", "status"]),
        filled_quantity: n(&["filled_quantity", "filledqty", "filled_qty"]) as i64,
        pending_quantity: n(&["pending_quantity", "pendingqty", "pending_qty"]) as i64,
        average_price: n(&["average_price", "averageprice", "avg_fill_price"]),
        rejection_reason: s(&["rejection_reason", "reject_reason"]),
        ..Default::default()
    }
}

/// The bus subscriber. Holds a weak reference so the module can be dropped.
pub struct StrategyOrderEvents {
    module: Weak<StrategyModule>,
}

#[async_trait::async_trait]
impl Subscriber for StrategyOrderEvents {
    fn name(&self) -> &'static str {
        "StrategyOrderUpdates"
    }

    fn topics(&self) -> Vec<Topic> {
        vec![Topic::OrderUpdate]
    }

    async fn handle(&self, event: Arc<Event>) {
        let Event::OrderUpdate(u) = &*event else {
            return;
        };
        if u.orderid.is_empty() {
            return;
        }
        if let Some(m) = self.module.upgrade() {
            m.apply_update(&u.orderid, u.clone()).await;
        }
    }
}

/// Subscribe the module to `order.update` (critical lane: a fill it never
/// hears about is a leg with no stop).
pub fn register(bus: &crate::events::EventBus, module: &Arc<StrategyModule>) {
    bus.subscribe(
        Arc::new(StrategyOrderEvents {
            module: Arc::downgrade(module),
        }),
        Lane::Critical,
    );
}

/// A broker status as the order row stores it: complete, cancelled,
/// rejected, or open for anything still working.
fn canonical_status(status: &str) -> &'static str {
    if FILLED.contains(&status) {
        "complete"
    } else if CANCELLED.contains(&status) {
        "cancelled"
    } else if DEAD.contains(&status) {
        "rejected"
    } else {
        "open"
    }
}

impl StrategyModule {
    /// Match one update to a strategy order and apply it.
    pub async fn apply_update(&self, order_id: &str, update: OrderUpdate) {
        let replays_before = self.order_events.replays();
        let row = match self.order_by_broker_id(order_id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Could not look up strategy order {}: {}", order_id, e);
                return;
            }
        };
        let (row, frames) = match row {
            Some(r) => (r, vec![update]),
            None => {
                // Somebody else's order, or ours a moment too early.
                if !self.order_events.hold(order_id, update, replays_before) {
                    return;
                }
                // A replay ran between the lookup and the hold: look again.
                let Ok(Some(r)) = self.order_by_broker_id(order_id).await else {
                    return;
                };
                let frames = self.order_events.take(order_id);
                if frames.is_empty() {
                    return;
                }
                (r, frames)
            }
        };
        for frame in frames {
            self.apply_frame(order_id, &row, &frame).await;
        }
    }

    /// Apply frames held before this order's row carried its broker id.
    pub async fn replay_for(&self, order_id: Option<&str>) {
        let Some(order_id) = order_id.filter(|o| !o.is_empty()) else {
            return;
        };
        let frames = self.order_events.bump_and_take(order_id);
        if frames.is_empty() {
            return;
        }
        let Ok(Some(row)) = self.order_by_broker_id(order_id).await else {
            return;
        };
        for frame in frames {
            self.apply_frame(order_id, &row, &frame).await;
        }
    }

    /// The strategy order carrying `broker_order_id`, read on the blocking
    /// pool: every order update and every status snapshot asks, from the
    /// runtime that judges the ticks (PROD-REL-02).
    async fn order_by_broker_id(
        &self,
        broker_order_id: &str,
    ) -> crate::error::Result<Option<OrderRow>> {
        let id = broker_order_id.to_string();
        self.store
            .blocking(move |s| s.get_order_by_broker_id(&id))
            .await
    }

    /// Fold one targeted order-status response through the push path.
    pub async fn apply_order_snapshot(&self, broker_order_id: &str, order: &Value) {
        let update = snapshot_frame(broker_order_id, order);
        if let Ok(Some(row)) = self.order_by_broker_id(&update.orderid).await {
            self.apply_frame(&update.orderid, &row, &update).await;
        }
    }

    /// Fold the order book's fact for `row`, a working order the
    /// reconciler's pass read with its run: no lookup, and no fold at all
    /// when the book says nothing the row does not, so a pass over orders
    /// that are still working reads nothing more (PROD-REL-02).
    pub(crate) async fn fold_book_fact(
        &self,
        row: &OrderRow,
        broker_order_id: &str,
        order: &Value,
    ) {
        let update = snapshot_frame(broker_order_id, order);
        let facts = FrameFacts::of(&update);
        if frame_changes(row, facts.status, facts.filled_qty) {
            self.apply_frame(&update.orderid, row, &update).await;
        }
    }

    /// Fold the frame into its order row on the blocking pool. The fold and
    /// whether an entry fill came after its run finished; `None` when the
    /// frame changed nothing or could not be folded (logged).
    async fn fold_off_runtime(
        &self,
        row: &OrderRow,
        facts: FrameFacts,
        is_entry: bool,
    ) -> Option<(OrderFactFold, bool)> {
        let (id, run_id) = (row.id, row.run_id);
        let folded = self
            .store
            .blocking(move |s| {
                let fold = match s.fold_order_broker_frame(
                    id,
                    facts.status,
                    facts.price,
                    facts.filled_qty,
                    facts.rejection.as_deref(),
                )? {
                    Some(f) if f.changed => f,
                    _ => return Ok(None),
                };
                let late_entry = is_entry
                    && fold.fill_delta > 0
                    && s.get_run(run_id)
                        .ok()
                        .flatten()
                        .is_some_and(|r| r.stopped_at.is_some());
                Ok(Some((fold, late_entry)))
            })
            .await;
        folded.unwrap_or_else(|e| {
            tracing::error!(
                "Could not fold broker facts for strategy order {}: {}",
                id,
                e
            );
            None
        })
    }

    async fn apply_frame(&self, order_id: &str, row: &OrderRow, u: &OrderUpdate) {
        let facts = FrameFacts::of(u);
        let canonical = facts.status;
        let is_entry = row.kind == "entry";
        // The fold waits for the database on the blocking pool, so two
        // frames of one order folded at once could reach the run the other
        // way round: an entry resized back to the older quantity, or an
        // exit fill finding its claim already released. Under the gate a
        // fill is applied before the next fold begins.
        let gate = self.fold_gate.lock().await;
        let Some((fold, late_entry)) = self.fold_off_runtime(row, facts, is_entry).await else {
            return;
        };
        let exit_owner = if is_entry {
            None
        } else {
            self.exit_owner_for_row(row.run_id, row.leg_id, row.id, row.position_ref.as_deref())
        };
        let should_apply =
            fold.fill_delta > 0 || (fold.terminal() && fold.cumulative_filled_qty > 0);
        let (price, opts) = fill_of(row, &fold, is_entry);
        let filled = (should_apply && !late_entry)
            .then(|| self.fill_state(row.run_id, row.leg_id, price, is_entry, &opts));
        drop(gate);
        if should_apply {
            self.after_frame_fill(
                row,
                order_id,
                &fold,
                (is_entry, late_entry),
                (price, filled),
            )
            .await;
        }

        let dead_transition =
            (canonical == "cancelled" || canonical == "rejected") && !fold.was_terminal();
        if dead_transition {
            tracing::warn!("Strategy order {} ended as {}", order_id, canonical);
            self.on_order_ended(row, canonical, &fold, is_entry, (should_apply, exit_owner))
                .await;
        }

        if fold.fill_delta > 0 || fold.terminal() {
            self.broadcast_order(row).await;
        }
    }

    /// What follows a frame's fill, once it is applied to the position it
    /// belongs to: the run's follow-ups (a late entry on a stopped run is
    /// corrected instead), and a fill the broker gave no price for is
    /// reported.
    async fn after_frame_fill(
        &self,
        row: &OrderRow,
        order_id: &str,
        fold: &OrderFactFold,
        (is_entry, late_entry): (bool, bool),
        (price, filled): (Option<f64>, Option<FillOutcome>),
    ) {
        if late_entry {
            self.manage_late_entry_correction(row.run_id).await;
        } else if let Some(outcome) = filled {
            self.after_fill(row.run_id, is_entry, outcome).await;
        }
        if fold.fill_delta > 0 && price.is_none() {
            self.report_unpriced_fill(row, order_id, fold.cumulative_filled_qty)
                .await;
        }
    }

    /// An order that just ended cancelled or rejected: an unfilled entry
    /// will never become a position; an exit's claim goes back, and a
    /// held, stopping or stranded exit is reported.
    async fn on_order_ended(
        &self,
        row: &OrderRow,
        canonical: &str,
        fold: &OrderFactFold,
        is_entry: bool,
        (should_apply, mut exit_owner): (bool, Option<String>),
    ) {
        if is_entry && fold.cumulative_filled_qty <= 0 {
            // Zero fill: the entry will never become a position.
            self.state.with_run(row.run_id, |run| {
                if let Some(leg) = run.leg_mut(row.leg_id) {
                    let owns = row.position_ref.is_none() || leg.position_ref == row.position_ref;
                    if owns && leg.entry_status != "complete" {
                        leg.entry_status = canonical.into();
                        leg.status = "rejected".into();
                    }
                }
            });
            self.reconcile_pending_stop(row.run_id).await;
        } else if !is_entry {
            if !should_apply {
                exit_owner = self
                    .state
                    .release_order_exit(row.run_id, row.leg_id, row.id, row.position_ref.as_deref())
                    .map(str::to_string);
            }
            let held = self.exit_owner_still_held(
                row.run_id,
                row.leg_id,
                exit_owner.as_deref(),
                row.position_ref.as_deref(),
            );
            if exit_owner.as_deref() == Some("superseded") && held {
                self.report_flip_outgoing_exit_rejected(
                    row.run_id,
                    row.leg_id,
                    canonical,
                    row.broker_order_id.as_deref(),
                )
                .await;
            }
            if held {
                self.report_pending_stop_exit_failed(row, canonical).await;
            }
            if fold.cumulative_filled_qty <= 0
                && exit_owner.is_none()
                && self.state.snapshot(row.run_id).is_none()
            {
                self.report_stranded_exit(row, canonical).await;
            }
        }
    }

    /// The order's durable row and the run's state, pushed to the page.
    async fn broadcast_order(&self, row: &OrderRow) {
        let (id, run_id) = (row.id, row.run_id);
        let read = self
            .store
            .blocking(move |s| Ok((s.get_order(id)?, s.get_run(run_id)?)))
            .await;
        if let Ok((Some(durable), Some(run))) = read {
            self.broadcast
                .push_order_update(run.strategy_id, durable.to_dict())
                .await;
        }
        self.broadcast
            .push_delta(self.state.snapshot(row.run_id), true)
            .await;
    }

    fn exit_owner_for_row(
        &self,
        run_id: i64,
        leg_id: i64,
        row_id: i64,
        position_ref: Option<&str>,
    ) -> Option<String> {
        self.state
            .with_run(run_id, |run| {
                let leg = run.leg(leg_id)?;
                if let Some(sup) = &leg.superseded {
                    if sup.exit_order_id == Some(row_id)
                        && (position_ref.is_none() || sup.position_ref.as_deref() == position_ref)
                    {
                        return Some("superseded".to_string());
                    }
                }
                if leg.exit_order_id == Some(row_id)
                    && (position_ref.is_none() || leg.position_ref.as_deref() == position_ref)
                {
                    return Some("live".to_string());
                }
                None
            })
            .flatten()
    }

    fn exit_owner_still_held(
        &self,
        run_id: i64,
        leg_id: i64,
        owner: Option<&str>,
        position_ref: Option<&str>,
    ) -> bool {
        self.state
            .with_run(run_id, |run| {
                let Some(leg) = run.leg(leg_id) else {
                    return false;
                };
                match owner {
                    Some("live") => {
                        (position_ref.is_none() || leg.position_ref.as_deref() == position_ref)
                            && leg.status == "open"
                            && leg.qty > 0
                    }
                    Some("superseded") => leg.superseded.as_ref().is_some_and(|s| {
                        (position_ref.is_none() || s.position_ref.as_deref() == position_ref)
                            && s.qty > 0
                    }),
                    _ => false,
                }
            })
            .unwrap_or(false)
    }

    /// A run and its strategy, read on the blocking pool; `None` when
    /// either is gone or cannot be read.
    async fn run_and_strategy(&self, run_id: i64) -> Option<(RunRow, StrategyRow)> {
        self.store
            .blocking(move |s| {
                let Some(run) = s.get_run(run_id)? else {
                    return Ok(None);
                };
                Ok(s.get_strategy_unscoped(run.strategy_id)?
                    .map(|strategy| (run, strategy)))
            })
            .await
            .ok()
            .flatten()
    }

    async fn report_unpriced_fill(&self, row: &OrderRow, order_id: &str, qty: i64) {
        let Some((run, strategy)) = self.run_and_strategy(row.run_id).await else {
            return;
        };
        self.emit(
            run.strategy_id,
            &strategy.user_id,
            if row.kind == "entry" {
                "leg_entry_placed"
            } else {
                "leg_exit_placed"
            },
            &format!(
                "Broker order {} reports {} filled on leg {} without a usable average price. The quantity is managed as reported, but its value and realized P&L cannot be verified; check the fill price with your broker.",
                order_id, qty, row.leg_id
            ),
            super::store::EventFields {
                run_id: Some(row.run_id),
                leg_id: Some(row.leg_id),
                severity: Some("critical"),
                payload: None,
            },
        )
        .await;
    }

    async fn report_pending_stop_exit_failed(&self, row: &OrderRow, ended: &str) {
        let Some((run, strategy)) = self.run_and_strategy(row.run_id).await else {
            return;
        };
        if run.stopped_at.is_some() || run.stop_requested_reason.is_none() {
            return;
        }
        let order = row
            .broker_order_id
            .as_ref()
            .map(|b| format!(" order {}", b))
            .unwrap_or_default();
        self.emit(
            run.strategy_id,
            &strategy.user_id,
            "run_stop_failed",
            &format!(
                "Stop exit{} for leg {} was {}. The position remains open and managed, and the stop can be retried.",
                order, row.leg_id, ended
            ),
            super::store::EventFields {
                run_id: Some(row.run_id),
                leg_id: Some(row.leg_id),
                severity: Some("critical"),
                payload: None,
            },
        )
        .await;
    }

    async fn report_stranded_exit(&self, row: &OrderRow, ended: &str) {
        let Some((run, strategy)) = self.run_and_strategy(row.run_id).await else {
            return;
        };
        self.emit(
            run.strategy_id,
            &strategy.user_id,
            "run_stop_failed",
            &format!(
                "Exit order {} for leg {} was {} after the run had already closed. The {} of {} {} did not happen, so that position is still held and nothing is managing it.",
                row.broker_order_id.clone().unwrap_or_default(),
                row.leg_id,
                ended,
                row.action,
                row.qty,
                row.symbol
            ),
            super::store::EventFields {
                run_id: Some(row.run_id),
                leg_id: Some(row.leg_id),
                severity: Some("critical"),
                payload: Some(json!({"broker_order_id": row.broker_order_id})),
            },
        )
        .await;
    }

    /// An entry fill that arrives after its run finished with zero exposure:
    /// reopen the run, rebuild it from durable facts and continue its stop.
    pub async fn manage_late_entry_correction(&self, run_id: i64) -> bool {
        let Ok(Some(run)) = self.store.get_run(run_id) else {
            return false;
        };
        let strategy_id = run.strategy_id;
        let user_id = self
            .store
            .get_strategy_unscoped(strategy_id)
            .ok()
            .flatten()
            .map(|s| s.user_id)
            .unwrap_or_default();
        if !super::recovery::reopen_run_for_late_entry_fill(&self.store, run_id).unwrap_or(false) {
            self.emit(
                strategy_id,
                &user_id,
                "run_stop_failed",
                "A late broker entry fill arrived for a run already closed as flat, and the run could not be reopened. Check this position with your broker now.",
                super::store::EventFields {
                    run_id: Some(run_id),
                    severity: Some("critical"),
                    ..Default::default()
                },
            )
            .await;
            return false;
        }
        let recovered = super::recovery::recover_run(self, run_id).await;
        if !recovered.ok {
            self.emit(
                strategy_id,
                &user_id,
                "run_stop_failed",
                "A late broker entry fill reopened this run, but its position could not be rebuilt automatically. Check this position with your broker now.",
                super::store::EventFields {
                    run_id: Some(run_id),
                    severity: Some("critical"),
                    ..Default::default()
                },
            )
            .await;
            return false;
        }
        self.follow_prices(run_id, &recovered.symbols).await;
        self.reconcile_pending_stop(run_id).await;
        true
    }
}
