//! Per-strategy orderbook, tradebook and positions for the Detail page (web
//! `services/strategy_module/views.py`).
//!
//! Read from the account's own books, then filtered to this strategy: for
//! money the broker is the authority, and the order rows decide only which
//! broker rows belong here. The envelope is passed through unchanged so the
//! same table components render it; only aggregates are recounted.
//!
//! The book comes from the RUN's mode, not the analyzer toggle: a sandbox run
//! reads the sandbox books, a live run the broker's. Only orders placed in
//! that mode are attributed to it (SM-03): a residual left by a run of the
//! other mode is not in this book, so it neither stamps a row with its run
//! nor enters the totals. The web matched owners of every mode by contract;
//! the Detail page shows the other mode's residuals apart.
//!
//! The strategy record the page reads also carries its realized P&L per
//! mode over every finalised run (`strategy_detail`, SM-05).
//!
//! Positions carry a weaker guarantee: a position row is per contract, so a
//! contract also held from elsewhere is shared and cannot be divided. A
//! strategy's P&L comes from its own fills, never from these rows.

use super::dispatch::{Book, RunMode};
use super::store::{ModedOrder, OrderRow, Store, StrategyRow};
use super::StrategyModule;
use crate::error::AppError;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

const DEFAULT_STATISTIC_KEYS: &[&str] = &[
    "total_buy_orders",
    "total_sell_orders",
    "total_completed_orders",
    "total_open_orders",
    "total_rejected_orders",
];
const POSITION_TOTALS: &[(&str, &str)] = &[
    ("total_pnl", "pnl"),
    ("total_pnl_today", "pnl"),
    ("total_unrealized_pnl", "unrealized_pnl"),
    ("total_today_realized_pnl", "today_realized_pnl"),
];

fn err(message: impl Into<String>) -> Value {
    json!({"status": "error", "message": message.into()})
}

fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

fn number(v: &Value) -> f64 {
    crate::risk::value_to_f64(v).unwrap_or(0.0)
}

fn rows(v: &Value) -> Vec<Value> {
    v.as_array()
        .map(|a| a.iter().filter(|r| r.is_object()).cloned().collect())
        .unwrap_or_default()
}

/// Recount order statistics over the filtered orders, keyed like the
/// service's own block.
pub fn statistics(orders: &[Value], template: &Value) -> Value {
    let (mut buy, mut sell) = (0, 0);
    let mut by: BTreeMap<String, i64> = BTreeMap::new();
    for o in orders {
        match text(&o["action"]).to_ascii_uppercase().as_str() {
            "BUY" => buy += 1,
            "SELL" => sell += 1,
            _ => {}
        }
        let mut s = text(
            o.get("order_status")
                .filter(|v| !v.is_null())
                .unwrap_or(&o["orderstatus"]),
        )
        .to_ascii_lowercase();
        if matches!(
            s.as_str(),
            "trigger pending" | "trigger_pending" | "trigger-pending"
        ) {
            s = "trigger pending".into();
        }
        *by.entry(s).or_default() += 1;
    }
    let get = |k: &str| *by.get(k).unwrap_or(&0);
    let counted: Map<String, Value> = [
        ("total_orders", orders.len() as i64),
        ("total_buy_orders", buy),
        ("total_sell_orders", sell),
        ("total_completed_orders", get("complete")),
        ("total_open_orders", get("open")),
        ("total_rejected_orders", get("rejected")),
        ("total_cancelled_orders", get("cancelled")),
        ("total_trigger_pending_orders", get("trigger pending")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), json!(v)))
    .collect();
    let keys: Vec<String> = match template.as_object() {
        Some(m) if !m.is_empty() => m.keys().cloned().collect(),
        _ => DEFAULT_STATISTIC_KEYS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    Value::Object(
        keys.into_iter()
            .map(|k| {
                let v = counted.get(&k).cloned().unwrap_or(json!(0));
                (k, v)
            })
            .collect(),
    )
}

fn filled_quantity(row: &OrderRow) -> i64 {
    match row.filled_qty {
        Some(q) if q > 0 => q,
        None if row.status.eq_ignore_ascii_case("complete")
            && row
                .broker_order_id
                .as_deref()
                .is_some_and(|b| !b.is_empty())
            && row.qty > 0 =>
        {
            row.qty
        }
        _ => 0,
    }
}

/// Lifetime local residuals, one per durable position owner.
fn residual_owners<'a>(
    orders: impl IntoIterator<Item = &'a OrderRow>,
    product: &str,
) -> Vec<Value> {
    type OwnerKey = (i64, i64, String, String, String, String);
    let mut groups: BTreeMap<OwnerKey, Vec<&OrderRow>> = BTreeMap::new();
    for o in orders {
        let q = filled_quantity(o);
        let action = o.action.to_ascii_uppercase();
        if q <= 0 || (action != "BUY" && action != "SELL") || o.symbol.is_empty() {
            continue;
        }
        let p = o
            .product
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| product.to_string())
            .to_ascii_uppercase();
        groups
            .entry((
                o.run_id,
                o.leg_id,
                o.position_ref.clone().unwrap_or_default(),
                o.symbol.to_ascii_uppercase(),
                o.exchange.to_ascii_uppercase(),
                p,
            ))
            .or_default()
            .push(o);
    }
    let mut out = Vec::new();
    for ((run_id, leg_id, pref, symbol, exchange, product), mut rows) in groups {
        rows.sort_by(|a, b| {
            (a.filled_at.as_deref().unwrap_or(&a.placed_at), a.id)
                .cmp(&(b.filled_at.as_deref().unwrap_or(&b.placed_at), b.id))
        });
        let mut lots: Vec<(i64, i64, Option<f64>)> = Vec::new();
        for r in rows {
            let side = if r.action.eq_ignore_ascii_case("BUY") {
                1
            } else {
                -1
            };
            let mut remaining = filled_quantity(r);
            while remaining > 0 && !lots.is_empty() && lots[0].0 != side {
                let m = remaining.min(lots[0].1);
                lots[0].1 -= m;
                remaining -= m;
                if lots[0].1 <= 0 {
                    lots.remove(0);
                }
            }
            if remaining > 0 {
                lots.push((side, remaining, r.avg_fill_price.filter(|p| *p > 0.0)));
            }
        }
        let net: i64 = lots.iter().map(|l| l.0 * l.1).sum();
        if net == 0 {
            continue;
        }
        let gross: i64 = lots.iter().map(|l| l.1).sum();
        let avg = if lots.iter().all(|l| l.2.is_some()) && gross > 0 {
            Some(
                lots.iter()
                    .map(|l| l.1 as f64 * l.2.unwrap_or(0.0))
                    .sum::<f64>()
                    / gross as f64,
            )
        } else {
            None
        };
        out.push(json!({
            "symbol": symbol, "exchange": exchange, "product": product,
            "quantity": net, "average_price": avg, "ltp": null, "pnl": null,
            "source": "local/unreconciled",
            "position_ref": if pref.is_empty() { Value::Null } else { json!(pref) },
            "run_id": run_id, "leg_id": leg_id,
        }));
    }
    out
}

fn contract_key(row: &Value, product: &str) -> (String, String, String) {
    let p = text(&row["product"]).to_ascii_uppercase();
    (
        text(&row["symbol"]).to_ascii_uppercase(),
        text(&row["exchange"]).to_ascii_uppercase(),
        if p.is_empty() { product.to_string() } else { p },
    )
}

fn overlay(broker_rows: Vec<Value>, owners: Vec<Value>, product: &str) -> Vec<Value> {
    let mut by_contract: BTreeMap<(String, String, String), Vec<Value>> = BTreeMap::new();
    for o in owners {
        by_contract
            .entry(contract_key(&o, product))
            .or_default()
            .push(o);
    }
    let mut result = Vec::new();
    let mut matched = HashSet::new();
    for row in broker_rows {
        let key = contract_key(&row, product);
        let Some(owners) = by_contract.get(&key) else {
            continue;
        };
        let rp = text(&row["product"]).to_ascii_uppercase();
        if !product.is_empty() && !rp.is_empty() && rp != product {
            continue;
        }
        matched.insert(key.clone());
        let mut m = row.as_object().cloned().unwrap_or_default();
        if owners.len() == 1 {
            m.insert("source".into(), json!("broker"));
            m.insert("position_ref".into(), owners[0]["position_ref"].clone());
            m.insert("run_id".into(), owners[0]["run_id"].clone());
            m.insert("leg_id".into(), owners[0]["leg_id"].clone());
        } else {
            m.insert("source".into(), json!("broker/shared"));
        }
        result.push(Value::Object(m));
    }
    for (key, owners) in by_contract {
        if !matched.contains(&key) || owners.len() > 1 {
            result.extend(owners);
        }
    }
    result
}

/// Why a view has no book to read.
enum Refusal {
    /// A request the trader can correct (an unknown or foreign run).
    Message(String),
    /// A local record could not be read (SM-04). Never "never run".
    Read(AppError),
}

impl From<AppError> for Refusal {
    fn from(e: AppError) -> Self {
        Refusal::Read(e)
    }
}

/// A strategy's order history, each row with its run's mode (SM-03), as
/// the books read it.
pub type History = Arc<Vec<ModedOrder>>;

/// The mode of the book a view reads; `None` when the strategy has never
/// run. A failed read is an error, not "never run".
fn view_mode(
    store: &Store,
    strategy_id: i64,
    run_id: Option<i64>,
) -> Result<Option<RunMode>, Refusal> {
    let mode = match run_id {
        Some(id) => {
            let run = store
                .get_run(id)?
                .ok_or_else(|| Refusal::Message(format!("Run {} was not found", id)))?;
            if run.strategy_id != strategy_id {
                return Err(Refusal::Message(format!(
                    "Run {} does not belong to strategy {}",
                    id, strategy_id
                )));
            }
            run.mode
        }
        None => match store.list_runs(strategy_id, 1)?.into_iter().next() {
            Some(r) => text(&r["mode"]),
            None => return Ok(None),
        },
    };
    RunMode::parse(&mode)
        .map(Some)
        .ok_or_else(|| Refusal::Message(format!("Unknown run mode: '{}'", mode)))
}

/// The orders of `history` placed in `mode` (SM-03).
fn placed_in(history: &[ModedOrder], mode: RunMode) -> impl Iterator<Item = &OrderRow> {
    history
        .iter()
        .filter(move |o| o.mode == mode.as_str())
        .map(|o| &o.order)
}

/// The broker order ids of the orders placed in `mode`.
fn broker_ids(history: &[ModedOrder], mode: RunMode) -> HashSet<String> {
    placed_in(history, mode)
        .filter_map(|r| r.broker_order_id.clone().filter(|b| !b.trim().is_empty()))
        .collect()
}

impl StrategyModule {
    /// The strategy's order history for its books, read off the async
    /// workers and shared by the books that ask for it at once: the Detail
    /// page polls all three every 5 s, and the history grows with every
    /// run. A failed read is an error: an empty set would filter every
    /// broker row out and show a book that looks flat.
    pub(crate) async fn order_history(
        &self,
        strategy_id: i64,
        run_id: Option<i64>,
    ) -> Result<History, AppError> {
        let store = self.store.clone();
        self.histories
            .share((strategy_id, run_id), || async move {
                store
                    .blocking(move |s| {
                        Ok(Arc::new(s.list_orders_for_strategy(strategy_id, run_id)?))
                    })
                    .await
            })
            .await
    }

    /// The broker order ids of the strategy's orders placed in `mode`,
    /// gathered off the async workers.
    async fn order_ids(
        &self,
        strategy_id: i64,
        run_id: Option<i64>,
        mode: RunMode,
    ) -> Result<HashSet<String>, AppError> {
        let history = self.order_history(strategy_id, run_id).await?;
        self.store
            .blocking(move |_| Ok(broker_ids(&history, mode)))
            .await
    }

    /// The book's mode, or the answer to give instead: `Ok(Err(answer))`
    /// for "never run" or a refused run, `Err` for a local read failure.
    /// Read off the async workers.
    async fn mode_or_answer(
        &self,
        strategy_id: i64,
        run_id: Option<i64>,
        never_run: Value,
    ) -> Result<Result<RunMode, Value>, AppError> {
        let found = self
            .store
            .blocking(move |s| Ok(view_mode(s, strategy_id, run_id)))
            .await?;
        Ok(match found {
            Ok(Some(m)) => Ok(m),
            Ok(None) => Err(never_run),
            Err(Refusal::Message(m)) => Err(err(m)),
            Err(Refusal::Read(e)) => return Err(e),
        })
    }

    /// The strategy `id` of `user`, read on the blocking pool: every
    /// strategy page and `/api/v1/strategy` request looks it up first, and
    /// the Detail page polls its books every 5 s. On the runtime the lookup
    /// waited for a pooled connection while the books' own reads held them
    /// (polled_reads: 114 and 173 ms on a macOS runner).
    pub async fn owned_strategy(
        &self,
        id: i64,
        user: &str,
    ) -> Result<Option<StrategyRow>, AppError> {
        let user = user.to_string();
        self.store
            .blocking(move |s| s.get_strategy(id, &user))
            .await
    }

    /// The account the API key belongs to, read on the blocking pool.
    pub async fn api_key_owner(&self) -> Result<Option<String>, AppError> {
        self.store.blocking(|s| s.api_key_owner()).await
    }

    /// The strategy as the Detail page reads it: its record and, under
    /// `realized_by_mode`, the realized P&L of every finalised run summed per
    /// mode over its whole history (SM-05). The page used to add up the at
    /// most 100 runs it had loaded, Live and Sandbox together.
    pub fn strategy_detail(&self, strategy: &StrategyRow) -> Result<Value, AppError> {
        let mut detail = strategy.to_dict(true);
        detail["realized_by_mode"] = self.store.realized_by_mode(strategy.id)?;
        Ok(detail)
    }

    fn as_error(response: Value, fallback: &str) -> Value {
        if response["status"] == "error" {
            return response;
        }
        let m = text(&response["message"]);
        err(if m.is_empty() {
            fallback.to_string()
        } else {
            m
        })
    }

    /// The strategy's orders from the run's book. `Err` only when a local
    /// record could not be read; broker failures are error envelopes.
    pub async fn strategy_orderbook(
        &self,
        strategy_id: i64,
        run_id: Option<i64>,
    ) -> Result<Value, AppError> {
        let never_run = json!({"status": "success", "data": {"orders": [], "statistics": statistics(&[], &Value::Null)}});
        let mode = match self.mode_or_answer(strategy_id, run_id, never_run).await? {
            Ok(m) => m,
            Err(answer) => return Ok(answer),
        };
        let ids = self.order_ids(strategy_id, run_id, mode).await?;
        let mut payload = match self.gateway.book(mode, Book::Orders).await {
            Ok(p) => p,
            Err(e) => return Ok(Self::as_error(e, "Could not read the orderbook")),
        };
        let mut data = payload["data"].as_object().cloned().unwrap_or_default();
        let orders: Vec<Value> = rows(data.get("orders").unwrap_or(&Value::Null))
            .into_iter()
            .filter(|o| ids.contains(&text(&o["orderid"])))
            .collect();
        let stats = statistics(&orders, data.get("statistics").unwrap_or(&Value::Null));
        data.insert("statistics".into(), stats);
        data.insert("orders".into(), Value::Array(orders));
        payload["data"] = Value::Object(data);
        Ok(payload)
    }

    /// The strategy's trades; `Err` only for a local read failure.
    pub async fn strategy_tradebook(
        &self,
        strategy_id: i64,
        run_id: Option<i64>,
    ) -> Result<Value, AppError> {
        let never_run = json!({"status": "success", "data": []});
        let mode = match self.mode_or_answer(strategy_id, run_id, never_run).await? {
            Ok(m) => m,
            Err(answer) => return Ok(answer),
        };
        let ids = self.order_ids(strategy_id, run_id, mode).await?;
        let mut payload = match self.gateway.book(mode, Book::Trades).await {
            Ok(p) => p,
            Err(e) => return Ok(Self::as_error(e, "Could not read the tradebook")),
        };
        payload["data"] = Value::Array(
            rows(&payload["data"])
                .into_iter()
                .filter(|t| ids.contains(&text(&t["orderid"])))
                .collect(),
        );
        Ok(payload)
    }

    /// The strategy's positions; `Err` only for a local read failure.
    pub async fn strategy_positions(
        &self,
        strategy_id: i64,
        run_id: Option<i64>,
    ) -> Result<Value, AppError> {
        let never_run = json!({"status": "success", "data": []});
        let mode = match self.mode_or_answer(strategy_id, run_id, never_run).await? {
            Ok(m) => m,
            Err(answer) => return Ok(answer),
        };
        // Lifetime owners of this mode: a prior run can still own a position.
        let history = self.order_history(strategy_id, None).await?;
        let (product, owners) = self
            .store
            .blocking(move |s| {
                // A failed read must not silently drop the product filter.
                let product = s
                    .get_strategy_unscoped(strategy_id)?
                    .map(|s| s.product.to_ascii_uppercase())
                    .unwrap_or_default();
                let owners = residual_owners(placed_in(&history, mode), &product);
                Ok((product, owners))
            })
            .await?;
        let mut payload = match self.gateway.book(mode, Book::Positions).await {
            Ok(p) => p,
            Err(e) => return Ok(Self::as_error(e, "Could not read the positions")),
        };
        let positions = overlay(rows(&payload["data"]), owners, &product);
        if let Some(m) = payload.as_object_mut() {
            for (key, field) in POSITION_TOTALS {
                if m.contains_key(*key) {
                    let total: f64 = positions.iter().map(|r| number(&r[*field])).sum();
                    m.insert((*key).into(), json!((total * 100.0).round() / 100.0));
                }
            }
            m.insert("data".into(), Value::Array(positions));
        }
        Ok(payload)
    }
}
