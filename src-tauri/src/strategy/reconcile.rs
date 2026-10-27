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
use super::store::{EventFields, OrderRow, RunRow, Store, StrategyRow};
use super::StrategyModule;
use crate::error::Result;
use crate::services::order_reconciler::{
    held_message, Absence, BookSnapshot, Found, HeldOrder, OrderOwner, Unconfirmed,
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

/// The open runs of `mode`.
fn open_runs_of(store: &Store, mode: RunMode) -> Result<Vec<RunRow>> {
    Ok(store
        .list_open_runs()?
        .into_iter()
        .filter(|r| RunMode::parse(&r.mode) == Some(mode))
        .collect())
}

/// One open run as a reconcile pass reads it: its orders and, when one of
/// them is unconfirmed, its strategy (none when it is gone) and the client
/// tag each unconfirmed order was sent with (its witness).
struct RunRead {
    orders: Vec<OrderRow>,
    strategy: Option<StrategyRow>,
    tags: HashMap<i64, String>,
}

/// Read one open run for a reconcile pass.
fn read_run(s: &Store, run: &RunRow) -> Result<RunRead> {
    let orders = s.list_orders(run.id)?;
    if !orders.iter().any(unconfirmed) {
        return Ok(RunRead {
            orders,
            strategy: None,
            tags: HashMap::new(),
        });
    }
    let events = s.list_events(
        run.strategy_id,
        Some(run.id),
        Some(ORDER_UNCONFIRMED),
        None,
        500,
    )?;
    let tags = events
        .iter()
        .filter_map(|e| {
            let p = e.get("payload")?;
            let id = p.get("order_id")?.as_i64()?;
            let tag = p.get("client_tag")?.as_str()?.to_string();
            Some((id, tag))
        })
        .collect();
    Ok(RunRead {
        orders,
        strategy: s.get_strategy_unscoped(run.strategy_id)?,
        tags,
    })
}

impl StrategyModule {
    /// The open runs of `mode`, each as a pass reads it (or why it could
    /// not be), all read at once on the blocking pool: the order reconciler
    /// asks every few seconds while a strategy order works, from an async
    /// worker, and matches in memory (PROD-REL-02).
    async fn open_runs_read(&self, mode: RunMode) -> Result<Vec<Result<RunRead>>> {
        self.store
            .blocking(move |s| {
                Ok(open_runs_of(s, mode)?
                    .iter()
                    .map(|run| read_run(s, run))
                    .collect())
            })
            .await
    }

    /// Whether any open run of `mode` has a working or unconfirmed order.
    /// Unknown counts as yes: a book is read rather than an order left
    /// unsettled.
    pub fn has_open_orders(&self, mode: RunMode) -> bool {
        let holds = || -> Result<bool> {
            for run in open_runs_of(&self.store, mode)? {
                if self
                    .store
                    .list_orders(run.id)?
                    .iter()
                    .any(|o| unconfirmed(o) || (has_broker_id(o) && working(&o.status)))
                {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        holds().unwrap_or_else(|e| {
            tracing::warn!("Could not check for open strategy orders: {}", e);
            true
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

    /// Which of these broker order ids already belong to a strategy order.
    /// Unknown counts as all of them, so no other owner binds one.
    pub fn owned_among(&self, order_ids: &[String]) -> HashSet<String> {
        self.store.owned_broker_ids(order_ids).unwrap_or_else(|e| {
            tracing::warn!("Strategy orders could not be read to tell their own: {}", e);
            order_ids.iter().cloned().collect()
        })
    }

    /// An unconfirmed order the book does not show: settled as never placed
    /// once that is certain, else held; a Live order is never released on
    /// absence alone (P0-3), and the trader is told when it is held.
    /// Answers how many orders it leaves unresolved (0 or 1).
    async fn judge_absent(
        &self,
        key: &str,
        o: &OrderRow,
        strategy: &StrategyRow,
        (mode, book): (RunMode, &BookSnapshot),
        u: &Unconfirmed<'_>,
    ) -> usize {
        let id = o.id.to_string();
        match self.reconcile_absence.judge(key, &id, mode, book, u).await {
            Absence::NotPlaced => {
                self.reconcile_absence.forget(key);
                self.settle_never_placed(o, strategy.id, &strategy.user_id)
                    .await;
                0
            }
            Absence::Wait => 1,
            Absence::Hold { alert } => {
                // Live: never released on absence alone (P0-3). The claim
                // stays; the trader decides, or corroboration releases it.
                if alert {
                    let what = format!("Strategy {}, leg {}", strategy.name, o.leg_id);
                    let msg = held_message(&what, u);
                    self.reconcile_absence.describe(key, &msg);
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
                1
            }
        }
    }

    /// Several book orders match an unconfirmed order and nothing tells
    /// them apart: reported once per order, and never sent again
    /// automatically.
    async fn report_ambiguous(&self, key: &str, o: &OrderRow, strategy: &StrategyRow, n: usize) {
        if self.reconcile_ambiguous.lock().insert(key.to_string()) {
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

    /// Fold one book read for every open run of `mode`. Returns how many
    /// unconfirmed placements remain unresolved, or why a run's orders could
    /// not be read (the other runs are folded all the same).
    pub async fn reconcile_book(&self, mode: RunMode, book: &BookSnapshot) -> Result<usize> {
        let mut unresolved = 0;
        let mut live_keys = HashSet::new();
        let mut unread = None;
        for run in self.open_runs_read(mode).await? {
            let run = match run {
                Ok(r) => r,
                Err(e) => {
                    unread.get_or_insert(e);
                    continue;
                }
            };
            // Working orders: fold the book's fact (idempotent), from the
            // rows this pass read, never one read per order.
            for o in run
                .orders
                .iter()
                .filter(|o| has_broker_id(o) && working(&o.status))
            {
                let bid = o.broker_order_id.clone().unwrap_or_default();
                if let Some(b) = book.by_id(&bid) {
                    self.fold_book_fact(o, &bid, &b.row).await;
                }
            }
            // Its unconfirmed orders; a run whose strategy is gone has no
            // one to settle them for.
            let Some(strategy) = &run.strategy else {
                continue;
            };
            for o in run.orders.iter().filter(|o| unconfirmed(o)) {
                let key = format!("{}:{}", mode.as_str(), o.id);
                live_keys.insert(key.clone());
                let u = Unconfirmed {
                    symbol: &o.symbol,
                    exchange: &o.exchange,
                    action: &o.action,
                    quantity: o.qty,
                    product: o.product.as_deref(),
                    client_tag: run.tags.get(&o.id).map(String::as_str),
                    placed_at: placed_at(o),
                    net_before: Some(net_before(o)),
                    pricetype: Some(&o.pricetype),
                    price: Some(o.price),
                    trigger_price: Some(o.trigger_price),
                };
                match book.find(&u, &|id: &str| book.owned(id)) {
                    Found::Order(b) => {
                        self.reconcile_absence.forget(&key);
                        book.claim(&b.order_id);
                        let row = b.row.clone();
                        let id = b.order_id.clone();
                        self.bind_unconfirmed(o, &id, &row, strategy.id, &strategy.user_id)
                            .await;
                    }
                    Found::Absent => {
                        unresolved += self.judge_absent(&key, o, strategy, (mode, book), &u).await;
                    }
                    Found::Ambiguous(n) => {
                        unresolved += 1;
                        self.report_ambiguous(&key, o, strategy, n).await;
                    }
                }
            }
        }
        // A run not read keeps what is known of its orders (its absences
        // and held orders) until a pass reads it.
        if let Some(e) = unread {
            return Err(e);
        }
        // Bounded by the rows still unconfirmed (this destination's only:
        // the other's pass keeps its own).
        let scope = format!("{}:", mode.as_str());
        self.reconcile_absence.retain_scope(&scope, &live_keys);
        self.reconcile_ambiguous
            .lock()
            .retain(|k| !k.starts_with(&scope) || live_keys.contains(k));
        Ok(unresolved)
    }

    async fn bind_unconfirmed(
        &self,
        o: &OrderRow,
        broker_order_id: &str,
        row: &Value,
        strategy_id: i64,
        user_id: &str,
    ) {
        // Written on the blocking pool, as the pass reads (PROD-REL-02).
        let (id, bound_to) = (o.id, broker_order_id.to_string());
        let bound = self
            .store
            .blocking(move |s| s.update_order(id, Some("open"), Some(bound_to.as_str()), None))
            .await;
        match bound {
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
    pub async fn confirm_not_placed(&self, id: &str) -> std::result::Result<bool, String> {
        let Some(key) = self.reconcile_absence.held_key(id) else {
            return Ok(false);
        };
        let Ok(order_id) = id.parse::<i64>() else {
            return Ok(false);
        };
        // Read on the blocking pool, under the reconciler's pass lock.
        let read =
            self.store
                .blocking(move |s| {
                    let Some(o) = s.get_order(order_id)? else {
                        return Ok(None);
                    };
                    let strategy =
                        s.get_run(o.run_id).ok().flatten().and_then(|run| {
                            s.get_strategy_unscoped(run.strategy_id).ok().flatten()
                        });
                    Ok(Some((o, strategy)))
                })
                .await;
        let (o, strategy) = match read {
            Ok(Some(found)) => found,
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
        let id = o.id;
        let _ = self
            .store
            .blocking(move |s| s.update_order(id, Some("rejected"), None, Some(NOT_PLACED)))
            .await;
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

    fn owned_among(&self, order_ids: &[String]) -> HashSet<String> {
        self.0
            .upgrade()
            .map(|m| m.owned_among(order_ids))
            .unwrap_or_default()
    }

    async fn reconcile(&self, mode: RunMode, book: &BookSnapshot) -> Result<usize> {
        match self.0.upgrade() {
            Some(m) => m.reconcile_book(mode, book).await,
            None => Ok(0),
        }
    }

    fn held_orders(&self) -> Vec<HeldOrder> {
        self.0
            .upgrade()
            .map(|m| m.held_orders())
            .unwrap_or_default()
    }

    async fn confirm_not_placed(&self, id: &str) -> std::result::Result<bool, String> {
        match self.0.upgrade() {
            Some(m) => m.confirm_not_placed(id).await,
            None => Ok(false),
        }
    }
}
