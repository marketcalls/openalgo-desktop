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
//!   accepted. Absent, [`AbsenceTracker::judge`] decides: in Sandbox an
//!   absence confirmed by [`ABSENT_READS`] reads over [`ABSENT_SPAN_SECS`]
//!   seconds rejects the row as never placed and releases the claim, so
//!   the leg is managed (and exitable) again. In Live absence alone never
//!   does (P0-3): the claim is held, a critical event and a Health alert
//!   are raised once, and the row is released when the trader marks it
//!   not placed (Health page, [`StrategyModule::confirm_not_placed`]) or
//!   when corroboration does. Several matching orders are never guessed
//!   between: the trader is told and the claim stays.
//!
//! [`AbsenceTracker::judge`]: crate::services::order_reconciler::AbsenceTracker::judge
//!
//! [`ABSENT_READS`]: crate::services::order_reconciler::ABSENT_READS
//! [`ABSENT_SPAN_SECS`]: crate::services::order_reconciler::ABSENT_SPAN_SECS

use super::dispatch::RunMode;
use super::engine::{ORDER_UNCONFIRMED, UNCONFIRMED};
use super::store::{EventFields, OrderRow, RunRow};
use super::StrategyModule;
use crate::services::order_reconciler::{
    find_placed, held_message, Absence, BookSnapshot, Found, HeldOrder, OrderOwner, Unconfirmed,
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

/// The leg's net position just before this order was sent: flat before an
/// entry; before an exit, the position it closes (a BUY exit closes a
/// short). The broker's net must still show exactly this for a Live
/// absence to be released without the trader (review P1-2).
fn net_before(o: &OrderRow) -> i64 {
    if o.kind == "entry" {
        0
    } else if o.action.eq_ignore_ascii_case("BUY") {
        -o.qty
    } else {
        o.qty
    }
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
            for o in rows
                .iter()
                .filter(|o| has_broker_id(o) && working(&o.status))
            {
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
                    net_before: Some(net_before(o)),
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
                        let id = o.id.to_string();
                        match self
                            .reconcile_absence
                            .judge(&key, &id, mode, book, &u)
                            .await
                        {
                            Absence::NotPlaced => {
                                self.reconcile_absence.forget(&key);
                                self.settle_never_placed(o, strategy.id, &strategy.user_id)
                                    .await;
                            }
                            Absence::Wait => unresolved += 1,
                            Absence::Hold { alert } => {
                                // Live: never released on absence alone
                                // (P0-3). The claim stays; the trader
                                // decides, or corroboration releases it.
                                unresolved += 1;
                                if alert {
                                    let what =
                                        format!("Strategy {}, leg {}", strategy.name, o.leg_id);
                                    let msg = held_message(&what, &u);
                                    self.reconcile_absence.describe(&key, &msg);
                                    book.alert_held(msg.clone());
                                    self.emit(
                                        strategy.id,
                                        &strategy.user_id,
                                        "order_unconfirmed_held",
                                        &msg,
                                        EventFields {
                                            run_id: Some(o.run_id),
                                            leg_id: Some(o.leg_id),
                                            severity: Some("critical"),
                                            payload: Some(json!({"order_id": o.id})),
                                        },
                                    )
                                    .await;
                                }
                            }
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
            .list_events(
                strategy_id,
                Some(run_id),
                Some(ORDER_UNCONFIRMED),
                None,
                500,
            )
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

    /// Live orders held for the trader (P0-3).
    pub fn held_orders(&self) -> Vec<HeldOrder> {
        self.reconcile_absence.held()
    }

    /// The trader checked the broker's books: the held order `id` (the
    /// strategy order row) was not placed. Releases it as the reconciler
    /// would. Run under the reconciler's pass lock, so no pass binds it
    /// meanwhile.
    pub async fn confirm_not_placed(&self, id: &str) -> Result<bool, String> {
        let Some(key) = self.reconcile_absence.held_key(id) else {
            return Ok(false);
        };
        let Ok(order_id) = id.parse::<i64>() else {
            return Ok(false);
        };
        let o = match self.store.get_order(order_id) {
            Ok(Some(o)) => o,
            Ok(None) => {
                self.reconcile_absence.forget(&key);
                return Ok(false);
            }
            Err(e) => {
                tracing::error!("Held strategy order {} could not be read: {}", id, e);
                return Err("The order could not be read. Try again.".into());
            }
        };
        if !unconfirmed(&o) {
            // Found or settled since the page was loaded.
            self.reconcile_absence.forget(&key);
            return Ok(false);
        }
        let strategy = self.store.get_run(o.run_id).ok().flatten().and_then(|run| {
            self.store
                .get_strategy_unscoped(run.strategy_id)
                .ok()
                .flatten()
        });
        let Some(strategy) = strategy else {
            return Err("The strategy of this order could not be read. Try again.".into());
        };
        self.reconcile_absence.forget(&key);
        self.settle_not_placed(&o, strategy.id, &strategy.user_id, true)
            .await;
        Ok(true)
    }

    async fn settle_never_placed(&self, o: &OrderRow, strategy_id: i64, user_id: &str) {
        self.settle_not_placed(o, strategy_id, user_id, false).await
    }

    async fn settle_not_placed(
        &self,
        o: &OrderRow,
        strategy_id: i64,
        user_id: &str,
        by_trader: bool,
    ) {
        let _ = self
            .store
            .update_order(o.id, Some("rejected"), None, Some(NOT_PLACED));
        if o.kind == "entry" {
            self.state
                .settle_unconfirmed_entry(o.run_id, o.leg_id, o.id, false);
        } else {
            self.state
                .release_order_exit(o.run_id, o.leg_id, o.id, o.position_ref.as_deref());
        }
        let why = if by_trader {
            "You marked it as not placed"
        } else {
            "The broker has no record of it after repeated checks"
        };
        self.emit(
            strategy_id,
            user_id,
            "order_unconfirmed_absent",
            &format!(
                "The unconfirmed {} {} {} for leg {} was not placed. {}. The leg is managed again{}.",
                o.action,
                o.qty,
                o.symbol,
                o.leg_id,
                why,
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

    fn held_orders(&self) -> Vec<HeldOrder> {
        self.0
            .upgrade()
            .map(|m| m.held_orders())
            .unwrap_or_default()
    }

    async fn confirm_not_placed(&self, id: &str) -> Result<bool, String> {
        match self.0.upgrade() {
            Some(m) => m.confirm_not_placed(id).await,
            None => Ok(false),
        }
    }
}
