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

use super::engine::FillOpts;
use super::store::{OrderFactFold, OrderRow};
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

impl StrategyModule {
    /// Match one update to a strategy order and apply it.
    pub async fn apply_update(&self, order_id: &str, update: OrderUpdate) {
        let replays_before = self.order_events.replays();
        let row = match self.store.get_order_by_broker_id(order_id) {
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
                let Ok(Some(r)) = self.store.get_order_by_broker_id(order_id) else {
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
        let Ok(Some(row)) = self.store.get_order_by_broker_id(order_id) else {
            return;
        };
        for frame in frames {
            self.apply_frame(order_id, &row, &frame).await;
        }
    }

    /// Fold one targeted order-status response through the push path.
    pub async fn apply_order_snapshot(&self, broker_order_id: &str, order: &Value) {
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
        let update = OrderUpdate {
            orderid: id.clone(),
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
        };
        if let Ok(Some(row)) = self.store.get_order_by_broker_id(&id) {
            self.apply_frame(&id, &row, &update).await;
        }
    }

    async fn apply_frame(&self, order_id: &str, row: &OrderRow, u: &OrderUpdate) {
        let status = normalise(&u.order_status);
        let canonical = if FILLED.contains(&status.as_str()) {
            "complete"
        } else if CANCELLED.contains(&status.as_str()) {
            "cancelled"
        } else if DEAD.contains(&status.as_str()) {
            "rejected"
        } else {
            "open"
        };
        let incoming_qty = (u.filled_quantity > 0).then_some(u.filled_quantity);
        let incoming_price = usable_price(Some(u.average_price));
        let rejection = (!u.rejection_reason.is_empty()).then_some(u.rejection_reason.as_str());
        let is_entry = row.kind == "entry";

        let fold = match self.store.fold_order_broker_frame(
            row.id,
            canonical,
            incoming_price,
            incoming_qty,
            rejection,
        ) {
            Ok(Some(f)) if f.changed => f,
            Ok(_) => return,
            Err(e) => {
                tracing::error!(
                    "Could not fold broker facts for strategy order {}: {}",
                    row.id,
                    e
                );
                return;
            }
        };

        let mut exit_owner = if is_entry {
            None
        } else {
            self.exit_owner_for_row(row.run_id, row.leg_id, row.id, row.position_ref.as_deref())
        };
        let should_apply =
            fold.fill_delta > 0 || (fold.terminal() && fold.cumulative_filled_qty > 0);
        let late_entry = is_entry
            && fold.fill_delta > 0
            && self
                .store
                .get_run(row.run_id)
                .ok()
                .flatten()
                .map(|r| r.stopped_at.is_some())
                .unwrap_or(false);
        if should_apply {
            let price = if is_entry {
                usable_price(fold.average_fill_price)
            } else {
                incremental_fill_price(&fold)
            };
            let opts = FillOpts {
                filled_qty: Some(fold.fill_delta),
                order_row_id: Some(row.id),
                position_ref: row.position_ref.clone(),
                cumulative_filled_qty: (is_entry
                    && (!fold.terminal() || fold.previous_filled_qty > 0))
                    .then_some(fold.cumulative_filled_qty),
                order_terminal: fold.terminal(),
                allow_prior_order_correction: fold.was_terminal(),
            };
            if late_entry {
                self.manage_late_entry_correction(row.run_id).await;
            } else {
                self.apply_fill(row.run_id, row.leg_id, price, is_entry, opts)
                    .await;
            }
            if fold.fill_delta > 0 && price.is_none() {
                self.report_unpriced_fill(row, order_id, fold.cumulative_filled_qty)
                    .await;
            }
        }

        let dead_transition =
            (canonical == "cancelled" || canonical == "rejected") && !fold.was_terminal();
        if dead_transition {
            tracing::warn!("Strategy order {} ended as {}", order_id, canonical);
            if is_entry && fold.cumulative_filled_qty <= 0 {
                // Zero fill: the entry will never become a position.
                self.state.with_run(row.run_id, |run| {
                    if let Some(leg) = run.leg_mut(row.leg_id) {
                        let owns =
                            row.position_ref.is_none() || leg.position_ref == row.position_ref;
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
                        .release_order_exit(
                            row.run_id,
                            row.leg_id,
                            row.id,
                            row.position_ref.as_deref(),
                        )
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

        if fold.fill_delta > 0 || fold.terminal() {
            if let (Ok(Some(durable)), Ok(Some(run))) =
                (self.store.get_order(row.id), self.store.get_run(row.run_id))
            {
                self.broadcast
                    .push_order_update(run.strategy_id, durable.to_dict())
                    .await;
            }
            self.broadcast
                .push_delta(self.state.snapshot(row.run_id), true)
                .await;
        }
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

    async fn report_unpriced_fill(&self, row: &OrderRow, order_id: &str, qty: i64) {
        let Ok(Some(run)) = self.store.get_run(row.run_id) else {
            return;
        };
        let Ok(Some(strategy)) = self.store.get_strategy_unscoped(run.strategy_id) else {
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
        let Ok(Some(run)) = self.store.get_run(row.run_id) else {
            return;
        };
        if run.stopped_at.is_some() || run.stop_requested_reason.is_none() {
            return;
        }
        let Ok(Some(strategy)) = self.store.get_strategy_unscoped(run.strategy_id) else {
            return;
        };
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
        let Ok(Some(run)) = self.store.get_run(row.run_id) else {
            return;
        };
        let Ok(Some(strategy)) = self.store.get_strategy_unscoped(run.strategy_id) else {
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
        self.feed.add_run(run_id, &recovered.symbols).await;
        self.reconcile_pending_stop(run_id).await;
        true
    }
}
