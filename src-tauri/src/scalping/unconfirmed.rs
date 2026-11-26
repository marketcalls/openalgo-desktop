//! Scalping page orders the destination did not confirm (LOG-08).
//!
//! An entry or a risk-reducing exit sent from the scalping page whose
//! answer is neither an acceptance nor a refusal may be at the broker. The
//! page used to get the raw error back, and a click to retry sent it again.
//! Now it is recorded here as unconfirmed, the trader is told to check the
//! order book, and every further order for that contract from the page is
//! refused until the order book settles it:
//!
//! * **found** in the book ([`BookSnapshot::find`]: by the client tag it was
//!   sent with where the broker reports tags, else by its fields and shape
//!   among orders nobody owns), it is settled as placed (an entry joins the
//!   scalping list);
//! * **absent**, [`AbsenceTracker::judge`] decides: in Sandbox a confirmed
//!   absence means it was not placed; in Live absence alone never releases
//!   it (a broker's book can lag; a wrong release lets a second order out).
//!   It is held, the trader is alerted once on the Health page, and it is
//!   released when the trader marks it not placed there. The page does not
//!   read the position before each order, so it gives no `net_before` and a
//!   Live order is never released without the trader.
//!
//! The rows live in `scalping_unconfirmed`, so all of this holds across a
//! restart.

use super::store::{MODE_ANALYZE, MODE_LIVE};
use crate::brokers::common::outcome::PlaceOutcome;
use crate::db::sqlite::SqliteDb;
use crate::error::Result;
use crate::services::core::Reply;
use crate::services::order_reconciler::{
    held_message, Absence, AbsenceTracker, BookSnapshot, Found, HeldOrder, OrderOwner, Unconfirmed,
};
use crate::state::AppState;
use crate::strategy::dispatch::RunMode;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Weak};

/// What the page is told when its order was not confirmed.
pub const NOT_CONFIRMED: &str =
    "The broker did not confirm this order. Check the order book before placing it again.";

/// What a repeat is told while an order for the contract is unconfirmed.
pub fn repeat_refusal(symbol: &str) -> String {
    format!(
        "An earlier order for {} was not confirmed by the broker, so nothing more is sent for it until the order book settles it. Check the order book.",
        symbol
    )
}

/// Refuse an order for a contract the page has an unconfirmed order for:
/// it may already be at the broker.
pub(crate) fn refuse_repeat(
    ctx: &AppState,
    mode: &str,
    symbol: &str,
    exchange: &str,
    product: &str,
) -> Option<Reply> {
    match open_for(&ctx.sqlite, mode, symbol, exchange, product) {
        Ok(false) => None,
        Ok(true) => Some(Reply::error(409, repeat_refusal(symbol))),
        Err(e) => {
            tracing::error!("Unconfirmed scalping orders could not be read: {}", e);
            Some(Reply::error(
                500,
                "The scalping orders could not be read just now, so nothing was sent. Try again in a moment.",
            ))
        }
    }
}

/// Record an order the broker did not confirm, so a retry is refused until
/// the order book settles it; the page's answer tells the trader so.
pub(crate) fn note(ctx: &AppState, p: &NewPending) -> Reply {
    if let Err(e) = record(&ctx.sqlite, p, ctx.now()) {
        tracing::error!(
            "The unconfirmed scalping {} {} {} could not be recorded: {}",
            p.action,
            p.quantity,
            p.symbol,
            e
        );
    }
    ctx.reconciler.request_pass();
    Reply::error(500, NOT_CONFIRMED)
}

/// The quantity of the split chunk with no definite answer: the last one
/// sent, as the chunks after it are reported not sent.
pub(crate) fn uncertain_chunk(r: &Reply) -> Option<i64> {
    r.body
        .get("results")?
        .as_array()?
        .iter()
        .rev()
        .find(|row| {
            row.get("message").and_then(Value::as_str)
                != Some(crate::services::batch_order_service::SPLIT_HELD)
        })?
        .get("quantity")?
        .as_i64()
}

/// The client tag an uncertain placement was sent with.
pub(crate) fn client_tag_of(r: &Reply) -> Option<String> {
    match &r.placement {
        Some(PlaceOutcome::Uncertain { client_tag, .. }) => client_tag.clone(),
        _ => None,
    }
}

/// Migration `085_scalping_unconfirmed`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS scalping_unconfirmed (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mode TEXT NOT NULL,
            kind TEXT NOT NULL,
            symbol TEXT NOT NULL,
            exchange TEXT NOT NULL,
            product TEXT NOT NULL,
            action TEXT NOT NULL,
            quantity INTEGER NOT NULL,
            client_tag TEXT,
            status TEXT NOT NULL DEFAULT 'unconfirmed',
            orderid TEXT,
            placed_at TEXT NOT NULL,
            settled_at TEXT
         );
         CREATE INDEX IF NOT EXISTS ix_scalping_unconfirmed_status
            ON scalping_unconfirmed(status, mode);",
    )?;
    Ok(())
}

/// The scalping store's word for a side.
pub fn mode_word(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Live => MODE_LIVE,
        RunMode::Sandbox => MODE_ANALYZE,
    }
}

/// One order from the page, as recorded when its answer was uncertain.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub id: i64,
    pub mode: String,
    /// `entry` or `exit`.
    pub kind: String,
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub action: String,
    pub quantity: i64,
    pub client_tag: Option<String>,
    pub placed_at: String,
}

/// What to record.
pub struct NewPending<'a> {
    pub mode: &'a str,
    pub kind: &'a str,
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub product: &'a str,
    pub action: &'a str,
    pub quantity: i64,
    pub client_tag: Option<&'a str>,
}

pub fn record(db: &SqliteDb, p: &NewPending, now: DateTime<Utc>) -> Result<i64> {
    let conn = db.conn()?;
    conn.execute(
        "INSERT INTO scalping_unconfirmed
            (mode, kind, symbol, exchange, product, action, quantity, client_tag, placed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            p.mode,
            p.kind,
            p.symbol,
            p.exchange,
            p.product,
            p.action,
            p.quantity,
            p.client_tag,
            now.to_rfc3339()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Whether the page has an unconfirmed order for this contract.
pub fn open_for(
    db: &SqliteDb,
    mode: &str,
    symbol: &str,
    exchange: &str,
    product: &str,
) -> Result<bool> {
    let conn = db.conn()?;
    Ok(conn
        .query_row(
            "SELECT 1 FROM scalping_unconfirmed WHERE status = 'unconfirmed' AND mode = ?1
                AND symbol = ?2 AND exchange = ?3 AND product = ?4 LIMIT 1",
            params![mode, symbol, exchange, product],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn pending_from_row(r: &rusqlite::Row) -> rusqlite::Result<Pending> {
    Ok(Pending {
        id: r.get(0)?,
        mode: r.get(1)?,
        kind: r.get(2)?,
        symbol: r.get(3)?,
        exchange: r.get(4)?,
        product: r.get(5)?,
        action: r.get(6)?,
        quantity: r.get(7)?,
        client_tag: r.get(8)?,
        placed_at: r.get(9)?,
    })
}

fn open_rows(conn: &Connection, mode: &str) -> Result<Vec<Pending>> {
    let mut st = conn.prepare_cached(
        "SELECT id, mode, kind, symbol, exchange, product, action, quantity, client_tag, placed_at
         FROM scalping_unconfirmed WHERE status = 'unconfirmed' AND mode = ?1 ORDER BY id",
    )?;
    let rows = st
        .query_map(params![mode], pending_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every unconfirmed order of one side.
pub fn pending(db: &SqliteDb, mode: &str) -> Result<Vec<Pending>> {
    let conn = db.conn()?;
    open_rows(&conn, mode)
}

fn settle(
    conn: &Connection,
    id: i64,
    status: &str,
    orderid: Option<&str>,
    now: DateTime<Utc>,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE scalping_unconfirmed SET status = ?2, orderid = ?3, settled_at = ?4
         WHERE id = ?1 AND status = 'unconfirmed'",
        params![id, status, orderid, now.to_rfc3339()],
    )? > 0)
}

fn read_one(conn: &Connection, id: i64) -> Result<Option<Pending>> {
    Ok(conn
        .query_row(
            "SELECT id, mode, kind, symbol, exchange, product, action, quantity, client_tag,
                placed_at FROM scalping_unconfirmed WHERE id = ?1 AND status = 'unconfirmed'",
            params![id],
            pending_from_row,
        )
        .optional()?)
}

/// An unconfirmed page order as the order book is searched for it.
fn unconfirmed_of(p: &Pending) -> Unconfirmed<'_> {
    Unconfirmed {
        symbol: &p.symbol,
        exchange: &p.exchange,
        action: &p.action,
        quantity: p.quantity,
        product: Some(p.product.as_str()),
        client_tag: p.client_tag.as_deref(),
        placed_at: DateTime::parse_from_rfc3339(&p.placed_at)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        // Not known: a Live absence is released by the trader only.
        net_before: None,
        // The page sends MARKET orders only.
        pricetype: Some("MARKET"),
        price: None,
        trigger_price: None,
    }
}

/// The scalping page as an owner of orders.
pub struct ScalpingOrders {
    ctx: Weak<AppState>,
    absence: AbsenceTracker,
    /// Orders matching several book orders the trader was told of.
    ambiguous: Mutex<HashSet<String>>,
}

impl ScalpingOrders {
    pub fn new(ctx: Weak<AppState>) -> Self {
        Self {
            ctx,
            absence: AbsenceTracker::new(),
            ambiguous: Mutex::new(HashSet::new()),
        }
    }
}

#[async_trait::async_trait]
impl OrderOwner for ScalpingOrders {
    fn name(&self) -> &'static str {
        "scalping-page"
    }

    fn has_open_orders(&self, mode: RunMode) -> bool {
        let Some(ctx) = self.ctx.upgrade() else {
            return false;
        };
        let read = ctx.sqlite.conn().and_then(|c| {
            let open = c
                .prepare_cached(
                    "SELECT 1 FROM scalping_unconfirmed WHERE status = 'unconfirmed' AND mode = ?1 LIMIT 1",
                )?
                .exists(params![mode_word(mode)])?;
            Ok(open)
        });
        // Unknown: read the book rather than leave an order unsettled.
        read.unwrap_or(true)
    }

    fn owned_among(&self, order_ids: &[String]) -> HashSet<String> {
        let Some(ctx) = self.ctx.upgrade() else {
            return HashSet::new();
        };
        let read = ctx.sqlite.conn().and_then(|c| {
            let mut st =
                c.prepare_cached("SELECT 1 FROM scalping_unconfirmed WHERE orderid = ?1 LIMIT 1")?;
            let mut owned = HashSet::new();
            for id in order_ids {
                if st.exists(params![id])? {
                    owned.insert(id.clone());
                }
            }
            Ok(owned)
        });
        // Unknown: every one counts as owned, so no other owner binds it.
        read.unwrap_or_else(|e| {
            tracing::warn!("Scalping orders could not be read to tell their own: {}", e);
            order_ids.iter().cloned().collect()
        })
    }

    async fn reconcile(&self, mode: RunMode, book: &BookSnapshot) -> Result<usize> {
        let Some(ctx) = self.ctx.upgrade() else {
            return Ok(0);
        };
        // On the blocking pool: a pass runs on an async worker (PROD-REL-02).
        let rows = ctx
            .blocking(move |c| pending(&c.sqlite, mode_word(mode)))
            .await?;
        let scope = format!("{}:", mode.as_str());
        let mut live: HashSet<String> = HashSet::new();
        let mut unresolved = 0;
        for p in &rows {
            let key = format!("{}{}", scope, p.id);
            live.insert(key.clone());
            let u = unconfirmed_of(p);
            match book.find(&u, &|id: &str| book.owned(id)) {
                Found::Order(b) => {
                    self.absence.forget(&key);
                    self.ambiguous.lock().remove(&key);
                    book.claim(&b.order_id);
                    self.settle_placed(&ctx, p, &b.order_id, book.read_at).await;
                }
                Found::Absent => {
                    let id = p.id.to_string();
                    match self.absence.judge(&key, &id, mode, book, &u).await {
                        Absence::Wait => unresolved += 1,
                        Absence::NotPlaced => {
                            self.absence.forget(&key);
                            let (row, at) = (p.id, book.read_at);
                            let settled = ctx
                                .blocking(move |c| {
                                    settle(&c.sqlite.conn()?, row, "not_placed", None, at)
                                })
                                .await;
                            match settled {
                                Ok(_) => tracing::info!(
                                    "The unconfirmed scalping {} {} {} is not in the order book after repeated checks: it was not placed.",
                                    p.action,
                                    p.quantity,
                                    p.symbol
                                ),
                                Err(e) => tracing::error!(
                                    "Unconfirmed scalping order {} could not be settled: {}",
                                    p.id,
                                    e
                                ),
                            }
                        }
                        Absence::Hold { alert } => {
                            // Live: never released on absence alone (P0-3).
                            unresolved += 1;
                            if alert {
                                let msg = held_message("Scalping page", &u);
                                self.absence.describe(&key, &msg);
                                book.alert_held(msg);
                            }
                        }
                    }
                }
                Found::Ambiguous(n) => {
                    unresolved += 1;
                    if self.ambiguous.lock().insert(key.clone()) {
                        tracing::error!(
                            "The unconfirmed scalping {} {} {} matches {} orders in the order book; none is taken for it. Check the order book.",
                            p.action,
                            p.quantity,
                            p.symbol,
                            n
                        );
                    }
                }
            }
        }
        // Bounded by the rows still unconfirmed on this side.
        self.absence.retain_scope(&scope, &live);
        self.ambiguous
            .lock()
            .retain(|k| !k.starts_with(&scope) || live.contains(k));
        Ok(unresolved)
    }

    fn held_orders(&self) -> Vec<HeldOrder> {
        self.absence.held()
    }

    /// The trader checked the broker's books: the held page order `id` was
    /// not placed, so the contract trades again. Run under the reconciler's
    /// pass lock, after it read the order book and trade book once more.
    async fn confirm_not_placed(&self, id: &str) -> std::result::Result<bool, String> {
        let Some(key) = self.absence.held_key(id) else {
            return Ok(false);
        };
        let Ok(row_id) = id.parse::<i64>() else {
            return Ok(false);
        };
        let Some(ctx) = self.ctx.upgrade() else {
            return Ok(false);
        };
        let now = ctx.now();
        // On the blocking pool, as the pass's own writes (PROD-REL-02).
        let marked = ctx
            .blocking(move |c| {
                let conn = c.sqlite.conn()?;
                let Some(p) = read_one(&conn, row_id)? else {
                    return Ok(None);
                };
                Ok(Some((p, settle(&conn, row_id, "not_placed", None, now)?)))
            })
            .await;
        match marked {
            Ok(Some((p, true))) => {
                self.absence.forget(&key);
                tracing::warn!(
                    "The trader marked the unconfirmed scalping {} {} {} as not placed",
                    p.action,
                    p.quantity,
                    p.symbol
                );
                Ok(true)
            }
            Ok(_) => {
                // Found or settled since the page was loaded.
                self.absence.forget(&key);
                Ok(false)
            }
            Err(e) => {
                tracing::error!("Held scalping order {} could not be marked: {}", id, e);
                Err(
                    "The order could not be marked just now. Nothing was changed. Try again."
                        .into(),
                )
            }
        }
    }
}

impl ScalpingOrders {
    /// The book shows the order: settle it as placed; an entry joins the
    /// scalping list. Both writes run on the blocking pool (PROD-REL-02):
    /// each commits to openalgo.db at `synchronous=FULL`, an fsync, and a
    /// slow disk held the pass's async worker, the one the feed and the
    /// risk owners run on, for as long as it took (lane-test 45750410715).
    async fn settle_placed(
        &self,
        ctx: &Arc<AppState>,
        p: &Pending,
        order_id: &str,
        at: DateTime<Utc>,
    ) {
        let (row, order) = (p.id, order_id.to_string());
        let entry = (p.kind == "entry").then(|| {
            (
                p.symbol.clone(),
                p.exchange.clone(),
                p.product.clone(),
                p.mode.clone(),
            )
        });
        let written = ctx
            .blocking(move |c| {
                if !settle(&c.sqlite.conn()?, row, "placed", Some(&order), at)? {
                    return Ok(None);
                }
                let tracked = entry.map(|(symbol, exchange, product, mode)| {
                    c.scalping.store.track(&symbol, &exchange, &product, &mode)
                });
                Ok(Some(tracked))
            })
            .await;
        match written {
            Ok(Some(Some(Err(e)))) => {
                tracing::error!("Scalping list could not record {}: {}", p.symbol, e);
            }
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(e) => {
                tracing::error!(
                    "Unconfirmed scalping order {} could not be settled as order {}: {}",
                    p.id,
                    order_id,
                    e
                );
                return;
            }
        }
        tracing::info!(
            "The unconfirmed scalping {} {} {} is order {} in the order book.",
            p.action,
            p.quantity,
            p.symbol,
            order_id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_created_twice_without_harm() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        assert!(repeat_refusal("NIFTY").contains("NIFTY"));
    }
}
