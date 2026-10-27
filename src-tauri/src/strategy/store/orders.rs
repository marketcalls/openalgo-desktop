//! Order rows: every durable order intent, the acknowledgement bind and
//! the order-fact fold (web `strategy_module_db.py` order functions).

use super::{
    order_from, NewOrder, OrderFactFold, OrderRow, Store, ORDER_COLS, TERMINAL_ORDER_STATUSES,
};
use crate::error::Result;
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;

/// A broker frame's status, lower case; anything unknown reads as working
/// (`open`).
fn frame_status(status: &str) -> String {
    let mut incoming = status.trim().to_ascii_lowercase();
    if !["open", "complete", "cancelled", "rejected"].contains(&incoming.as_str()) {
        incoming = "open".into();
    }
    incoming
}

/// What folding one frame into an order row would leave: the row's status
/// before and after, and its filled quantity before and after.
struct FoldFacts {
    previous_status: String,
    next_status: String,
    previous_qty: i64,
    cumulative: i64,
}

impl FoldFacts {
    /// Quantity evidence is cumulative: a frame never lowers it.
    fn of(row: &OrderRow, status: &str, filled_qty: Option<i64>) -> Self {
        let incoming = frame_status(status);
        let mut evidence = filled_qty.unwrap_or(0).max(0);
        if incoming == "complete" && evidence <= 0 {
            // A complete order with an omitted quantity traded in full.
            evidence = row.qty.max(0);
        }
        let previous_status = row.status.trim().to_ascii_lowercase();
        let previous_qty = row.filled_qty.unwrap_or(0).max(0);
        let cumulative = previous_qty.max(evidence);
        let next_status = folded_status(&previous_status, &incoming, cumulative - previous_qty);
        Self {
            previous_status,
            next_status,
            previous_qty,
            cumulative,
        }
    }
}

/// The row's status after a frame: `complete` stays; a dead row comes back
/// only as `complete` with strictly more filled quantity (`delta`); a
/// working row takes a terminal frame's status, else stays `open`.
fn folded_status(previous_status: &str, incoming: &str, delta: i64) -> String {
    if previous_status == "complete" {
        "complete".to_string()
    } else if previous_status == "cancelled" || previous_status == "rejected" {
        if incoming == "complete" && delta > 0 {
            "complete".to_string()
        } else {
            previous_status.to_string()
        }
    } else if TERMINAL_ORDER_STATUSES.contains(&incoming) {
        incoming.to_string()
    } else {
        "open".to_string()
    }
}

impl Store {
    /// Write an order row at placement time, before the broker answers.
    pub fn record_order(&self, run_id: i64, leg_id: i64, kind: &str, o: &NewOrder) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sm_strategy_order (run_id, leg_id, kind, position_ref, broker_order_id, \
             symbol, exchange, action, qty, product, pricetype, price, trigger_price, status, \
             placed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, 0, ?12, ?13)",
            params![
                run_id,
                leg_id,
                kind,
                o.position_ref,
                o.broker_order_id,
                o.symbol,
                o.exchange,
                o.action,
                o.qty,
                o.product,
                if o.pricetype.is_empty() {
                    "MARKET"
                } else {
                    &o.pricetype
                },
                if o.status.is_empty() {
                    "pending"
                } else {
                    &o.status
                },
                self.utcnow(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Write the broker's answer onto an order row. False when no row moved.
    pub fn update_order(
        &self,
        order_id: i64,
        status: Option<&str>,
        broker_order_id: Option<&str>,
        reject_reason: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn()?;
        let now = self.utcnow();
        let n = conn.execute(
            "UPDATE sm_strategy_order SET \
               status = COALESCE(?1, status), \
               filled_at = CASE WHEN ?1 = 'complete' AND filled_at IS NULL THEN ?2 ELSE filled_at END, \
               broker_order_id = COALESCE(?3, broker_order_id), \
               reject_reason = COALESCE(?4, reject_reason) \
             WHERE id = ?5",
            params![status, now, broker_order_id, reject_reason, order_id],
        )?;
        Ok(n == 1)
    }

    pub fn get_order(&self, order_id: i64) -> Result<Option<OrderRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM sm_strategy_order WHERE id = ?1", ORDER_COLS),
                [order_id],
                order_from,
            )
            .optional()?)
    }

    /// The strategy order carrying this broker reference (the "is it ours"
    /// test on every order update).
    pub fn get_order_by_broker_id(&self, broker_order_id: &str) -> Result<Option<OrderRow>> {
        if broker_order_id.is_empty() {
            return Ok(None);
        }
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM sm_strategy_order WHERE broker_order_id = ?1 ORDER BY id LIMIT 1",
                    ORDER_COLS
                ),
                [broker_order_id],
                order_from,
            )
            .optional()?)
    }

    /// Which of these broker references belong to a strategy order (the
    /// order reconciler's "is it someone's" test, once per book read).
    pub fn owned_broker_ids(&self, broker_order_ids: &[String]) -> Result<HashSet<String>> {
        let conn = self.conn()?;
        let mut st = conn
            .prepare_cached("SELECT 1 FROM sm_strategy_order WHERE broker_order_id = ?1 LIMIT 1")?;
        let mut owned = HashSet::new();
        for id in broker_order_ids.iter().filter(|i| !i.is_empty()) {
            if st.exists([id])? {
                owned.insert(id.clone());
            }
        }
        Ok(owned)
    }

    pub fn list_orders(&self, run_id: i64) -> Result<Vec<OrderRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_order WHERE run_id = ?1 ORDER BY placed_at ASC, id ASC",
            ORDER_COLS
        ))?;
        let rows = stmt
            .query_map([run_id], order_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Atomically fold one cumulative broker frame into an order row (web
    /// `fold_order_broker_frame`). Quantity evidence is cumulative, a working
    /// frame never reopens a terminal row, and a dead row is upgraded only by
    /// a `complete` frame carrying strictly more filled quantity. The UPDATE
    /// is conditional on the facts this read saw; a loser retries.
    pub fn fold_order_broker_frame(
        &self,
        order_id: i64,
        status: &str,
        avg_fill_price: Option<f64>,
        filled_qty: Option<i64>,
        reject_reason: Option<&str>,
    ) -> Result<Option<OrderFactFold>> {
        for _ in 0..8 {
            let Some(row) = self.get_order(order_id)? else {
                return Ok(None);
            };
            let raw_status = row.status.clone();
            let previous_price = row.avg_fill_price;
            let FoldFacts {
                previous_status,
                next_status,
                previous_qty,
                cumulative,
            } = FoldFacts::of(&row, status, filled_qty);
            let delta = cumulative - previous_qty;
            if next_status == previous_status && delta <= 0 {
                return Ok(Some(OrderFactFold {
                    order_id,
                    previous_status: previous_status.clone(),
                    status: previous_status,
                    previous_filled_qty: previous_qty,
                    cumulative_filled_qty: previous_qty,
                    fill_delta: 0,
                    previous_average_fill_price: previous_price,
                    average_fill_price: previous_price,
                    changed: false,
                }));
            }
            let set_fill = delta > 0;
            let set_filled_at = next_status == "complete" && previous_status != "complete";
            let set_reason = reject_reason.is_some()
                && (next_status == "cancelled" || next_status == "rejected");
            let conn = self.conn()?;
            let n = conn.execute(
                "UPDATE sm_strategy_order SET status = ?1, \
                   filled_qty = CASE WHEN ?2 THEN ?3 ELSE filled_qty END, \
                   avg_fill_price = CASE WHEN ?2 THEN ?4 ELSE avg_fill_price END, \
                   filled_at = CASE WHEN ?5 THEN ?6 ELSE filled_at END, \
                   reject_reason = CASE WHEN ?7 THEN ?8 ELSE reject_reason END \
                 WHERE id = ?9 AND status = ?10 AND filled_qty IS ?11",
                params![
                    next_status,
                    set_fill,
                    cumulative,
                    avg_fill_price,
                    set_filled_at,
                    self.utcnow(),
                    set_reason,
                    reject_reason,
                    order_id,
                    raw_status,
                    row.filled_qty,
                ],
            )?;
            if n != 1 {
                continue;
            }
            return Ok(Some(OrderFactFold {
                order_id,
                previous_status,
                status: next_status,
                previous_filled_qty: previous_qty,
                cumulative_filled_qty: cumulative,
                fill_delta: delta,
                previous_average_fill_price: previous_price,
                average_fill_price: if set_fill {
                    avg_fill_price
                } else {
                    previous_price
                },
                changed: true,
            }));
        }
        tracing::error!(
            "Could not fold broker facts for strategy order {} after concurrent updates",
            order_id
        );
        Ok(None)
    }

    /// Repair one exact pending row from its durable acknowledgement witness.
    /// Returns `repaired`, `already_bound`, `conflict` or `missing`.
    pub fn bind_order_acknowledgement(
        &self,
        order_id: i64,
        run_id: i64,
        leg_id: i64,
        broker_order_id: Option<&str>,
        status: &str,
        reject_reason: Option<&str>,
    ) -> Result<&'static str> {
        let desired = status.trim().to_ascii_lowercase();
        if desired != "open" && desired != "rejected" {
            return Ok("conflict");
        }
        let desired_id = broker_order_id.map(str::trim).filter(|s| !s.is_empty());
        if desired == "open" && desired_id.is_none() {
            return Ok("conflict");
        }
        let Some(row) = self
            .get_order(order_id)?
            .filter(|r| r.run_id == run_id && r.leg_id == leg_id)
        else {
            return Ok("missing");
        };
        let current_id = row
            .broker_order_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if current_id.is_some() && current_id != desired_id {
            return Ok("conflict");
        }
        if let Some(id) = desired_id {
            if let Some(other) = self.get_order_by_broker_id(id)? {
                if other.id != order_id {
                    return Ok("conflict");
                }
            }
        }
        let current_status = row.status.trim().to_ascii_lowercase();
        if current_status != "pending" {
            if current_id == desired_id
                && (desired == "open" || (desired == "rejected" && current_status == "rejected"))
            {
                return Ok("already_bound");
            }
            return Ok("conflict");
        }
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE sm_strategy_order SET status = ?1, \
               broker_order_id = COALESCE(?2, broker_order_id), \
               reject_reason = CASE WHEN ?1 = 'rejected' THEN ?3 ELSE reject_reason END \
             WHERE id = ?4 AND run_id = ?5 AND leg_id = ?6 AND status = ?7 \
               AND broker_order_id IS ?8",
            params![
                desired,
                desired_id,
                reject_reason,
                order_id,
                run_id,
                leg_id,
                row.status,
                row.broker_order_id
            ],
        )?;
        Ok(if n == 1 { "repaired" } else { "conflict" })
    }
}
