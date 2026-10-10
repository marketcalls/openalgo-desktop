//! The per-strategy position book (web `database/strategy_book_db.py` and
//! `subscribers/strategy_book_subscriber.py`).
//!
//! The broker nets positions per contract and knows nothing about which
//! strategy opened them. This keeps a parallel book keyed by the `strategy`
//! tag, fed entirely from the event bus, so the order path is untouched:
//!
//! * `order.placed` (and the batch completion events, whose legs publish no
//!   per-leg `order.placed`) record `orderid -> strategy`;
//! * `order.update` fills book the UNSEEN portion of each fill (the applied
//!   quantity and notional are watermarked per order, so duplicates and
//!   partials are exact).
//!
//! A fill that beats its tag (a sandbox MARKET order fills before
//! `order.placed` is published) is buffered and drained when the tag lands;
//! buffered fills expire after 10 minutes and tags after 30 days, so neither
//! table grows without bound. Tagging and booking share one lock, so the two
//! orderings are the only possible ones and both are handled.

use crate::db::sqlite::SqliteDb;
use crate::error::Result;
use crate::events::{Event, Lane, Subscriber, Topic};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::sync::Arc;

pub const TAG_RETENTION_DAYS: i64 = 30;
pub const PENDING_FILL_TTL_MINUTES: i64 = 10;
const FILLABLE: &[&str] = &["complete", "filled", "partially filled", "partial"];

/// Migration `073_strategy_book`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS strategy_order_tags (
            id INTEGER PRIMARY KEY,
            orderid VARCHAR(64) NOT NULL UNIQUE,
            user_id VARCHAR(64) NOT NULL,
            strategy VARCHAR(120) NOT NULL,
            symbol VARCHAR(64) NOT NULL,
            exchange VARCHAR(20) NOT NULL,
            product VARCHAR(20) NOT NULL,
            applied_quantity FLOAT NOT NULL DEFAULT 0.0,
            applied_notional FLOAT NOT NULL DEFAULT 0.0,
            created_at DATETIME NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_strategy_order_tags_orderid ON strategy_order_tags (orderid);
        CREATE INDEX IF NOT EXISTS ix_strategy_order_tags_user_id ON strategy_order_tags (user_id);
        CREATE INDEX IF NOT EXISTS ix_strategy_order_tags_strategy ON strategy_order_tags (strategy);
        CREATE TABLE IF NOT EXISTS strategy_pending_fills (
            id INTEGER PRIMARY KEY,
            orderid VARCHAR(64) NOT NULL,
            filled_quantity FLOAT NOT NULL,
            average_price FLOAT NOT NULL,
            action VARCHAR(10) NOT NULL,
            created_at DATETIME NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_strategy_pending_fills_orderid ON strategy_pending_fills (orderid);
        CREATE TABLE IF NOT EXISTS strategy_positions (
            id INTEGER PRIMARY KEY,
            user_id VARCHAR(64) NOT NULL,
            strategy VARCHAR(120) NOT NULL,
            symbol VARCHAR(64) NOT NULL,
            exchange VARCHAR(20) NOT NULL,
            product VARCHAR(20) NOT NULL,
            quantity FLOAT NOT NULL DEFAULT 0.0,
            average_price FLOAT NOT NULL DEFAULT 0.0,
            realized_pnl FLOAT NOT NULL DEFAULT 0.0,
            today_realized_pnl FLOAT NOT NULL DEFAULT 0.0,
            trade_date VARCHAR(10),
            updated_at DATETIME NOT NULL,
            CONSTRAINT uq_strategy_leg UNIQUE (user_id, strategy, symbol, exchange, product)
        );
        CREATE INDEX IF NOT EXISTS ix_strategy_positions_user_id ON strategy_positions (user_id);
        CREATE INDEX IF NOT EXISTS ix_strategy_positions_strategy ON strategy_positions (strategy);
        CREATE INDEX IF NOT EXISTS ix_strategy_positions_symbol ON strategy_positions (symbol);",
    )?;
    Ok(())
}

/// The book, over the main database.
pub struct StrategyBook {
    db: Arc<SqliteDb>,
    clock: Arc<dyn crate::clock::Clock>,
    session: (u32, u32),
    /// Serialises the watermark read-modify-write and the tag/drain pair.
    /// Crash atomicity of each fold comes from its transaction, not this.
    lock: Mutex<()>,
}

impl StrategyBook {
    pub fn new(
        db: Arc<SqliteDb>,
        clock: Arc<dyn crate::clock::Clock>,
        session: (u32, u32),
    ) -> Self {
        Self {
            db,
            clock,
            session,
            lock: Mutex::new(()),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    fn session_date(&self) -> String {
        super::session::session_day(self.now(), self.session.0, self.session.1).to_string()
    }

    /// Drop tags past retention and buffered fills past their TTL.
    pub fn prune(&self) -> Result<()> {
        let conn = self.db.conn()?;
        let tags = super::store::ts(self.now() - chrono::Duration::days(TAG_RETENTION_DAYS));
        let fills =
            super::store::ts(self.now() - chrono::Duration::minutes(PENDING_FILL_TTL_MINUTES));
        conn.execute(
            "DELETE FROM strategy_order_tags WHERE created_at < ?1",
            [tags],
        )?;
        conn.execute(
            "DELETE FROM strategy_pending_fills WHERE created_at < ?1",
            [fills],
        )?;
        Ok(())
    }

    /// Remember which strategy placed an order (ignores duplicates), then
    /// apply any fills that arrived before the tag.
    pub fn record_order_tag(
        &self,
        orderid: &str,
        user_id: &str,
        strategy: &str,
        symbol: &str,
        exchange: &str,
        product: &str,
    ) -> Result<bool> {
        if orderid.is_empty() || strategy.is_empty() {
            return Ok(false);
        }
        let _g = self.lock.lock();
        let mut conn = self.db.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO strategy_order_tags (orderid, user_id, strategy, symbol, \
             exchange, product, applied_quantity, applied_notional, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, ?7)",
            params![
                orderid,
                user_id,
                strategy,
                symbol,
                exchange,
                product,
                super::store::ts(self.now())
            ],
        )?;
        let pending: Vec<(i64, f64, f64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, filled_quantity, average_price, action FROM strategy_pending_fills \
                 WHERE orderid = ?1 ORDER BY id",
            )?;
            let rows = stmt
                .query_map([orderid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (id, qty, price, action) in pending {
            // The position, its watermark and the buffered row move together
            // (DB-02): a failure or crash part-way leaves all three as they
            // were, so the next drain books the fill once, never twice.
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            self.apply_fill_locked(&tx, orderid, qty, price, &action)?;
            tx.execute("DELETE FROM strategy_pending_fills WHERE id = ?1", [id])?;
            tx.commit()?;
        }
        Ok(true)
    }

    /// Book the unseen portion of a fill. `None` when the order is unknown
    /// (buffered) or the fill adds nothing new.
    pub fn apply_fill(
        &self,
        orderid: &str,
        filled_quantity: f64,
        average_price: f64,
        action: &str,
    ) -> Result<Option<Value>> {
        let _g = self.lock.lock();
        let mut conn = self.db.conn()?;
        // One transaction for the position and the watermark (DB-02): the
        // process lock orders callers, but only the transaction keeps a crash
        // between the two writes from booking the same fill again on replay.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let booked =
            self.apply_fill_locked(&tx, orderid, filled_quantity, average_price, action)?;
        tx.commit()?;
        Ok(booked)
    }

    /// Fold one fill. Callers run it inside a transaction: the position
    /// upsert and the watermark update below must commit together.
    fn apply_fill_locked(
        &self,
        conn: &Connection,
        orderid: &str,
        filled_quantity: f64,
        average_price: f64,
        action: &str,
    ) -> Result<Option<Value>> {
        type Tag = (i64, String, String, String, String, String, f64, f64);
        let tag: Option<Tag> = conn
            .query_row(
                "SELECT id, user_id, strategy, symbol, exchange, product, applied_quantity, \
                 applied_notional FROM strategy_order_tags WHERE orderid = ?1",
                [orderid],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((tag_id, user, strategy, symbol, exchange, product, applied_q, applied_n)) = tag
        else {
            if !orderid.is_empty() {
                conn.execute(
                    "INSERT INTO strategy_pending_fills (orderid, filled_quantity, average_price, \
                     action, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        orderid,
                        filled_quantity.abs(),
                        average_price,
                        action,
                        super::store::ts(self.now())
                    ],
                )?;
            }
            return Ok(None);
        };
        let filled = filled_quantity.abs();
        let delta = filled - applied_q;
        if delta <= 0.0 {
            return Ok(None);
        }
        // `average_price` is cumulative: price the delta from the change in
        // notional, or partials at different levels corrupt the cost basis.
        let cumulative_notional = filled * average_price;
        let price = (cumulative_notional - applied_n) / delta;
        let signed = if action.eq_ignore_ascii_case("BUY") {
            delta
        } else {
            -delta
        };
        let today = self.session_date();
        let leg: Option<(f64, f64, f64, f64, Option<String>)> = conn
            .query_row(
                "SELECT quantity, average_price, realized_pnl, today_realized_pnl, trade_date \
                 FROM strategy_positions WHERE user_id = ?1 AND strategy = ?2 AND symbol = ?3 \
                 AND exchange = ?4 AND product = ?5",
                params![user, strategy, symbol, exchange, product],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let (mut qty, mut avg, mut realized, mut today_realized, trade_date) =
            leg.unwrap_or((0.0, 0.0, 0.0, 0.0, Some(today.clone())));
        if trade_date.as_deref() != Some(today.as_str()) {
            today_realized = 0.0;
        }
        if qty == 0.0 || (qty > 0.0) == (signed > 0.0) {
            let total = qty.abs() + signed.abs();
            avg = (avg * qty.abs() + price * signed.abs()) / total;
            qty += signed;
        } else {
            let closing = signed.abs().min(qty.abs());
            let direction = if qty > 0.0 { 1.0 } else { -1.0 };
            let r = closing * (price - avg) * direction;
            realized += r;
            today_realized += r;
            let remaining = signed.abs() - closing;
            qty += signed;
            if qty.abs() < 1e-9 {
                qty = 0.0;
                avg = 0.0;
            } else if remaining > 0.0 {
                avg = price;
            }
        }
        conn.execute(
            "INSERT INTO strategy_positions (user_id, strategy, symbol, exchange, product, \
             quantity, average_price, realized_pnl, today_realized_pnl, trade_date, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(user_id, strategy, symbol, exchange, product) DO UPDATE SET \
             quantity = excluded.quantity, average_price = excluded.average_price, \
             realized_pnl = excluded.realized_pnl, today_realized_pnl = excluded.today_realized_pnl, \
             trade_date = excluded.trade_date, updated_at = excluded.updated_at",
            params![
                user,
                strategy,
                symbol,
                exchange,
                product,
                qty,
                avg,
                realized,
                today_realized,
                today,
                super::store::ts(self.now())
            ],
        )?;
        conn.execute(
            "UPDATE strategy_order_tags SET applied_quantity = ?1, applied_notional = ?2 WHERE id = ?3",
            params![filled, cumulative_notional, tag_id],
        )?;
        let r4 = |v: f64| (v * 10000.0).round() / 10000.0;
        Ok(Some(json!({
            "strategy": strategy, "symbol": symbol, "exchange": exchange, "product": product,
            "quantity": r4(qty), "average_price": r4(avg), "realized_pnl": r4(realized),
            "today_realized_pnl": r4(today_realized), "booked_quantity": r4(delta),
        })))
    }

    /// Every tracked leg, optionally narrowed. `today_realized_pnl` reads 0
    /// once the session date rolls over.
    pub fn get_strategy_legs(
        &self,
        user_id: Option<&str>,
        strategy: Option<&str>,
    ) -> Result<Vec<Value>> {
        let today = self.session_date();
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT strategy, symbol, exchange, product, quantity, average_price, realized_pnl, \
             today_realized_pnl, trade_date, updated_at FROM strategy_positions \
             WHERE (?1 IS NULL OR user_id = ?1) AND (?2 IS NULL OR strategy = ?2) ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![user_id, strategy], |r| {
                let trade_date: Option<String> = r.get(8)?;
                let today_pnl: f64 = r.get(7)?;
                Ok(json!({
                    "strategy": (r.get::<_, String>(0)?),
                    "symbol": (r.get::<_, String>(1)?),
                    "exchange": (r.get::<_, String>(2)?),
                    "product": (r.get::<_, String>(3)?),
                    "quantity": (r.get::<_, f64>(4)?),
                    "average_price": (r.get::<_, f64>(5)?),
                    "realized_pnl": (r.get::<_, f64>(6)?),
                    "today_realized_pnl": if trade_date.as_deref() == Some(today.as_str()) { today_pnl } else { 0.0 },
                    "updated_at": super::store::iso((r.get::<_, Option<String>>(9)?).as_deref()),
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn pending_fill_count(&self) -> Result<i64> {
        let conn = self.db.conn()?;
        Ok(
            conn.query_row("SELECT COUNT(*) FROM strategy_pending_fills", [], |r| {
                r.get(0)
            })?,
        )
    }
}

/// The bus subscriber (critical lane: a tag or fill it never saw is a wrong
/// position).
pub struct StrategyBookSubscriber {
    book: Arc<StrategyBook>,
}

impl StrategyBookSubscriber {
    pub fn new(book: Arc<StrategyBook>) -> Self {
        Self { book }
    }

    fn user_id(meta_request: &Value) -> String {
        meta_request
            .get("user_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    fn handle_sync(&self, event: &Event) -> Result<()> {
        match event {
            Event::OrderPlaced {
                meta,
                strategy,
                symbol,
                exchange,
                product,
                orderid,
                ..
            } => {
                let strategy = strategy.trim();
                if orderid.is_empty() || strategy.is_empty() {
                    return Ok(());
                }
                self.book.record_order_tag(
                    orderid,
                    &Self::user_id(&meta.request_data),
                    strategy,
                    symbol,
                    exchange,
                    product,
                )?;
            }
            Event::OrderUpdate(u) => {
                let status = u.order_status.trim().to_ascii_lowercase().replace('_', " ");
                if !FILLABLE.contains(&status.as_str()) {
                    return Ok(());
                }
                let filled = if u.filled_quantity != 0 {
                    u.filled_quantity
                } else {
                    u.quantity
                };
                if filled == 0 {
                    return Ok(());
                }
                let price = if u.average_price != 0.0 {
                    u.average_price
                } else {
                    u.price
                };
                self.book
                    .apply_fill(&u.orderid, filled as f64, price, &u.action)?;
            }
            Event::BasketCompleted { meta, strategy, .. } => {
                self.tag_batch(meta, strategy.as_deref().unwrap_or(""), None)?
            }
            Event::MultiOrderCompleted { meta, strategy, .. } => {
                self.tag_batch(meta, strategy.as_deref().unwrap_or(""), None)?
            }
            Event::SplitCompleted {
                meta,
                symbol,
                exchange,
                product,
                ..
            } => {
                let strategy = meta.request_data["strategy"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                self.tag_batch(
                    meta,
                    &strategy,
                    Some((symbol.clone(), exchange.clone(), product.clone())),
                )?
            }
            Event::OptionsCompleted {
                meta,
                symbol,
                exchange,
                product,
                ..
            } => {
                let strategy = meta.request_data["strategy"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                self.tag_batch(
                    meta,
                    &strategy,
                    Some((
                        Some(symbol.clone()),
                        Some(exchange.clone()),
                        product.clone(),
                    )),
                )?
            }
            _ => {}
        }
        Ok(())
    }

    /// Tag the child orders of a batch from its results list.
    fn tag_batch(
        &self,
        meta: &crate::events::OrderMeta,
        strategy: &str,
        defaults: Option<(Option<String>, Option<String>, Option<String>)>,
    ) -> Result<()> {
        let strategy = strategy.trim();
        if strategy.is_empty() {
            return Ok(());
        }
        let user = Self::user_id(&meta.request_data);
        let (ds, de, dp) = defaults.unwrap_or((None, None, None));
        let default_product = dp
            .or_else(|| meta.request_data["product"].as_str().map(str::to_string))
            .unwrap_or_default();
        let results = meta.response_data["results"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for leg in results {
            let pick = |k: &str, d: &Option<String>| {
                leg[k]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| d.clone())
                    .unwrap_or_default()
            };
            let symbol = pick("symbol", &ds);
            let exchange = pick("exchange", &de);
            let product = pick("product", &Some(default_product.clone()));
            if let Some(children) = leg["split_results"].as_array().filter(|c| !c.is_empty()) {
                for child in children {
                    let id = child["orderid"]
                        .as_str()
                        .or(child["order_id"].as_str())
                        .unwrap_or("");
                    if !id.is_empty() {
                        self.book
                            .record_order_tag(id, &user, strategy, &symbol, &exchange, &product)?;
                    }
                }
                continue;
            }
            let id = leg["orderid"]
                .as_str()
                .or(leg["order_id"].as_str())
                .unwrap_or("");
            if id.is_empty() {
                continue;
            }
            if symbol.is_empty() || exchange.is_empty() || product.is_empty() {
                tracing::warn!(
                    "Strategy book: skipping batch leg {} for {}: incomplete identity",
                    id,
                    strategy
                );
                continue;
            }
            self.book
                .record_order_tag(id, &user, strategy, &symbol, &exchange, &product)?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Subscriber for StrategyBookSubscriber {
    fn name(&self) -> &'static str {
        "StrategyBook"
    }

    fn topics(&self) -> Vec<Topic> {
        vec![
            Topic::OrderPlaced,
            Topic::OrderUpdate,
            Topic::BasketCompleted,
            Topic::SplitCompleted,
            Topic::OptionsCompleted,
            Topic::MultiOrderCompleted,
        ]
    }

    async fn handle(&self, event: Arc<Event>) {
        if let Err(e) = self.handle_sync(&event) {
            tracing::error!(
                "Strategy book could not handle '{}': {}",
                event.topic().as_str(),
                e
            );
        }
    }
}

/// Register the book subscriber on the critical lane.
pub fn register(bus: &crate::events::EventBus, book: Arc<StrategyBook>) {
    bus.subscribe(Arc::new(StrategyBookSubscriber::new(book)), Lane::Critical);
}
