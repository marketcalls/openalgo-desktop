//! Strategy orders against one broker order-book read (the order
//! reconciler's strategy owner; LOG-08, ARCH-01).
//!
//! * **Working orders with a broker id** are folded through the same path
//!   as an `order.update` frame (`apply_order_snapshot`). The row's
//!   cumulative facts guard the fold, so a fill the feed already delivered
//!   is not booked twice, and one the feed lost is booked once.
//! * **Unconfirmed placements** (rows marked `unconfirmed`: sent, no
//!   definite answer) keep their claim until settled here. Found in the
//!   book, the row is bound to that order and folded; an entry becomes
//!   accepted. Absent from [`ABSENT_READS`] reads over at least
//!   [`ABSENT_SPAN_SECS`] seconds, the row is rejected as never placed and
//!   the claim released, so the leg is managed (and exitable) again.
//!   Several matching orders are never guessed between: the trader is told
//!   and the claim stays.
//!
//! [`ABSENT_READS`]: crate::services::order_reconciler::ABSENT_READS
//! [`ABSENT_SPAN_SECS`]: crate::services::order_reconciler::ABSENT_SPAN_SECS

use super::dispatch::RunMode;
use super::engine::{ORDER_UNCONFIRMED, UNCONFIRMED};
use super::store::{EventFields, OrderRow, RunRow};
use super::StrategyModule;
use crate::services::order_reconciler::{
    find_placed, BookSnapshot, Found, OrderOwner, Unconfirmed,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Weak;

/// Never placed: what an unconfirmed row reads once the book has settled it.
pub const NOT_PLACED: &str = "The broker has no record of this order: it was not placed";

fn working(status: &str) -> bool {
    status.eq_ignore_ascii_case(UNCONFIRMED) || super::recovery::order_is_working(status)
}

fn has_broker_id(o: &OrderRow) -> bool {
    o.broker_order_id
        .as_deref()
        .is_some_and(|b| !b.trim().is_empty())
}

fn unconfirmed(o: &OrderRow) -> bool {
    !has_broker_id(o) && o.status.eq_ignore_ascii_case(UNCONFIRMED)
}

fn placed_at(o: &OrderRow) -> Option<chrono::DateTime<chrono::Utc>> {
    for f in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(&o.placed_at, f) {
            return Some(n.and_utc());
        }
    }
    None
}

impl StrategyModule {
    fn open_runs_of(&self, mode: RunMode) -> Vec<RunRow> {
        self.store
            .list_open_runs()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| RunMode::parse(&r.mode) == Some(mode))
            .collect()
    }

    /// Whether any open run of `mode` has a working or unconfirmed order.
    pub fn has_open_orders(&self, mode: RunMode) -> bool {
        self.open_runs_of(mode).iter().any(|run| {
            self.store
                .list_orders(run.id)
                .unwrap_or_default()
                .iter()
                .any(|o| unconfirmed(o) || (has_broker_id(o) && working(&o.status)))
        })
    }

    /// Unconfirmed rows still waiting (health and tests).
    pub fn unconfirmed_orders(&self) -> usize {
        self.store
            .list_open_runs()
            .unwrap_or_default()
            .iter()
            .map(|run| {
                self.store
                    .list_orders(run.id)
                    .unwrap_or_default()
                    .iter()
                    .filter(|o| unconfirmed(o))
                    .count()
            })
            .sum()
    }

    /// Whether a broker order id already belongs to a strategy order.
    pub fn owns_order(&self, order_id: &str) -> bool {
        matches!(self.store.get_order_by_broker_id(order_id), Ok(Some(_)))
    }

    /// Fold one book read for every open run of `mode`. Returns how many
    /// unconfirmed placements remain unresolved.
    pub async fn reconcile_book(&self, mode: RunMode, book: &BookSnapshot) -> usize {
        let mut unresolved = 0;
        let mut live_keys = HashSet::new();
        let mut claimed: HashSet<String> = HashSet::new();
        for run in self.open_runs_of(mode) {
            let rows = match self.store.list_orders(run.id) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(
                        "Strategy orders of run {} could not be read to reconcile: {}",
                        run.id,
                        e
                    );
                    continue;
                }
            };
            // Working orders: fold the book's fact (idempotent).
            for o in rows.iter().filter(|o| has_broker_id(o) && working(&o.status)) {
                let bid = o.broker_order_id.clone().unwrap_or_default();
                if let Some(b) = book.by_id(&bid) {
                    self.apply_order_snapshot(&bid, &b.row).await;
                }
            }
            let pending: Vec<&OrderRow> = rows.iter().filter(|o| unconfirmed(o)).collect();
            if pending.is_empty() {
                continue;
            }
            let strategy = match self.store.get_strategy_unscoped(run.strategy_id) {
                Ok(Some(s)) => s,
                _ => continue,
            };
            let tags = self.unconfirmed_tags(run.strategy_id, run.id);
            for o in pending {
                let key = format!("{}:{}", mode.as_str(), o.id);
                live_keys.insert(key.clone());
                let u = Unconfirmed {
                    symbol: &o.symbol,
                    exchange: &o.exchange,
                    action: &o.action,
                    quantity: o.qty,
                    product: o.product.as_deref(),
                    client_tag: tags.get(&o.id).map(String::as_str),
                    placed_at: placed_at(o),
                };
                let bound = |id: &str| claimed.contains(id) || book.owned(id);
                match find_placed(&book.rows, &u, &bound) {
                    Found::Order(b) => {
                        self.reconcile_absence.forget(&key);
                        claimed.insert(b.order_id.clone());
                        let row = b.row.clone();
                        let id = b.order_id.clone();
                        self.bind_unconfirmed(o, &id, &row, strategy.id, &strategy.user_id)
                            .await;
                    }
                    Found::Absent => {
                        if self.reconcile_absence.absent(&key, book.read_at) {
                            self.reconcile_absence.forget(&key);
                            self.settle_never_placed(o, strategy.id, &strategy.user_id)
                                .await;
                        } else {
                            unresolved += 1;
                        }
                    }
                    Found::Ambiguous(n) => {
                        unresolved += 1;
                        if self.reconcile_ambiguous.lock().insert(key.clone()) {
                            self.emit(
                                strategy.id,
                                &strategy.user_id,
                                "order_unconfirmed_ambiguous",
                                &format!(
                                    "{} orders in the broker's order book match the unconfirmed {} {} {} for leg {}, and nothing tells them apart. Check the order book; this leg is not sent again automatically.",
                                    n, o.action, o.qty, o.symbol, o.leg_id
                                ),
                                EventFields {
                                    run_id: Some(o.run_id),
                                    leg_id: Some(o.leg_id),
                                    severity: Some("critical"),
                                    payload: Some(json!({"order_id": o.id, "candidates": n})),
                                },
                            )
                            .await;
                        }
                    }
                }
            }
        }
        // Bounded by the rows still unconfirmed (this destination's only:
        // the other's pass keeps its own).
        let scope = format!("{}:", mode.as_str());
        self.reconcile_absence.retain_scope(&scope, &live_keys);
        self.reconcile_ambiguous
            .lock()
            .retain(|k| !k.starts_with(&scope) || live_keys.contains(k));
        unresolved
    }

    /// The client tag each unconfirmed row was sent with (its witness).
    fn unconfirmed_tags(&self, strategy_id: i64, run_id: i64) -> HashMap<i64, String> {
        self.store
            .list_events(strategy_id, Some(run_id), Some(ORDER_UNCONFIRMED), None, 500)
            .unwrap_or_default()
            .iter()
            .filter_map(|e| {
                let p = e.get("payload")?;
                let id = p.get("order_id")?.as_i64()?;
                let tag = p.get("client_tag")?.as_str()?.to_string();
                Some((id, tag))
            })
            .collect()
    }

    async fn bind_unconfirmed(
        &self,
        o: &OrderRow,
        broker_order_id: &str,
        row: &Value,
        strategy_id: i64,
        user_id: &str,
    ) {
        match self
            .store
            .update_order(o.id, Some("open"), Some(broker_order_id), None)
        {
            Ok(true) => {}
            other => {
                tracing::error!(
                    "Unconfirmed strategy order row {} could not be bound to broker order {}: {:?}",
                    o.id,
                    broker_order_id,
                    other.err()
                );
                return;
            }
        }
        if o.kind == "entry" {
            self.state
                .settle_unconfirmed_entry(o.run_id, o.leg_id, o.id, true);
        }
        self.emit(
            strategy_id,
            user_id,
            "order_unconfirmed_resolved",
            &format!(
                "The unconfirmed {} {} {} for leg {} was found in the broker's order book as order {}; it is managed from here.",
                o.action, o.qty, o.symbol, o.leg_id, broker_order_id
            ),
            EventFields {
                run_id: Some(o.run_id),
                leg_id: Some(o.leg_id),
                severity: Some("info"),
                payload: Some(json!({"order_id": o.id, "broker_order_id": broker_order_id})),
            },
        )
        .await;
        // Frames the feed delivered before the row carried the id, then the
        // book's own fact.
        self.replay_for(Some(broker_order_id)).await;
        self.apply_order_snapshot(broker_order_id, row).await;
    }

    async fn settle_never_placed(&self, o: &OrderRow, strategy_id: i64, user_id: &str) {
        let _ = self
            .store
            .update_order(o.id, Some("rejected"), None, Some(NOT_PLACED));
        if o.kind == "entry" {
            self.state
                .settle_unconfirmed_entry(o.run_id, o.leg_id, o.id, false);
        } else {
            self.state.release_order_exit(
                o.run_id,
                o.leg_id,
                o.id,
                o.position_ref.as_deref(),
            );
        }
        self.emit(
            strategy_id,
            user_id,
            "order_unconfirmed_absent",
            &format!(
                "The unconfirmed {} {} {} for leg {} is not in the broker's order book after repeated checks, so it was not placed. The leg is managed again{}.",
                o.action,
                o.qty,
                o.symbol,
                o.leg_id,
                if o.kind == "entry" { "" } else { " and can be exited" }
            ),
            EventFields {
                run_id: Some(o.run_id),
                leg_id: Some(o.leg_id),
                severity: Some("warn"),
                payload: Some(json!({"order_id": o.id})),
            },
        )
        .await;
        // A durable stop may be waiting on exactly this.
        self.reconcile_pending_stop(o.run_id).await;
    }
}

/// The strategy module as an owner of orders.
pub struct StrategyOrders(pub Weak<StrategyModule>);

#[async_trait::async_trait]
impl OrderOwner for StrategyOrders {
    fn name(&self) -> &'static str {
        "strategy"
    }

    fn has_open_orders(&self, mode: RunMode) -> bool {
        self.0.upgrade().is_some_and(|m| m.has_open_orders(mode))
    }

    fn owns_order(&self, order_id: &str) -> bool {
        self.0.upgrade().is_some_and(|m| m.owns_order(order_id))
    }

    async fn reconcile(&self, mode: RunMode, book: &BookSnapshot) -> usize {
        match self.0.upgrade() {
            Some(m) => m.reconcile_book(mode, book).await,
            None => 0,
        }
    }
}
