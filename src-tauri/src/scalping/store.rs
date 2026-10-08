//! Persistence for the scalping terminal (web `database/scalping_db.py`), in
//! the main SQLite database with the web's table and column names:
//!
//! * `scalping_sl_state`: one row per leg whose stop loss, target or trailing
//!   stop is managed, unique on `(symbol, exchange, product, mode)`;
//! * `scalping_tracked_symbol`: the instruments the terminal has traded (its
//!   "scalping list"), unique on the same key, which scopes Close All.
//!
//! `mode` is `analyze` (sandbox) or `live`; the same leg can hold one row in
//! each without either overwriting the other.

use crate::db::sqlite::SqliteDb;
use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};
use std::sync::Arc;

pub const MODE_ANALYZE: &str = "analyze";
pub const MODE_LIVE: &str = "live";

/// Migration `074_scalping`: both tables, created only when missing.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS scalping_sl_state (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            symbol VARCHAR(60) NOT NULL,
            exchange VARCHAR(10) NOT NULL,
            product VARCHAR(10) NOT NULL,
            side VARCHAR(4) NOT NULL DEFAULT 'BUY',
            mode VARCHAR(10) NOT NULL DEFAULT 'analyze',
            entry_price FLOAT NOT NULL DEFAULT 0.0,
            quantity INTEGER NOT NULL DEFAULT 0,
            initial_sl FLOAT,
            trailing_enabled BOOLEAN NOT NULL DEFAULT 0,
            trailing_step FLOAT,
            highest_price FLOAT,
            lowest_price FLOAT,
            current_sl FLOAT,
            target FLOAT,
            is_active BOOLEAN NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            CONSTRAINT uq_scalping_sl_leg UNIQUE (symbol, exchange, product, mode)
        );
        CREATE TABLE IF NOT EXISTS scalping_tracked_symbol (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            symbol VARCHAR(60) NOT NULL,
            exchange VARCHAR(10) NOT NULL,
            product VARCHAR(10) NOT NULL,
            mode VARCHAR(10) NOT NULL DEFAULT 'analyze',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            CONSTRAINT uq_scalping_tracked UNIQUE (symbol, exchange, product, mode)
        );",
    )?;
    Ok(())
}

/// One managed leg (the web's `_to_dict`).
#[derive(Debug, Clone, PartialEq)]
pub struct SlState {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub mode: String,
    pub side: String,
    pub entry_price: f64,
    pub quantity: i64,
    pub initial_sl: Option<f64>,
    pub trailing_enabled: bool,
    pub trailing_step: Option<f64>,
    pub highest_price: Option<f64>,
    pub lowest_price: Option<f64>,
    pub current_sl: Option<f64>,
    pub target: Option<f64>,
    pub is_active: bool,
}

impl SlState {
    pub fn to_dict(&self) -> Value {
        json!({
            "symbol": self.symbol,
            "exchange": self.exchange,
            "product": self.product,
            "mode": self.mode,
            "side": self.side,
            "entry_price": self.entry_price,
            "quantity": self.quantity,
            "initial_sl": self.initial_sl,
            "trailing_enabled": self.trailing_enabled,
            "trailing_step": self.trailing_step,
            "highest_price": self.highest_price,
            "lowest_price": self.lowest_price,
            "current_sl": self.current_sl,
            "target": self.target,
            "is_active": self.is_active,
        })
    }

    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            symbol: r.get("symbol")?,
            exchange: r.get("exchange")?,
            product: r.get("product")?,
            mode: r.get("mode")?,
            side: r.get("side")?,
            entry_price: r.get::<_, Option<f64>>("entry_price")?.unwrap_or(0.0),
            quantity: r.get::<_, Option<i64>>("quantity")?.unwrap_or(0),
            initial_sl: r.get("initial_sl")?,
            trailing_enabled: r
                .get::<_, Option<bool>>("trailing_enabled")?
                .unwrap_or(false),
            trailing_step: r.get("trailing_step")?,
            highest_price: r.get("highest_price")?,
            lowest_price: r.get("lowest_price")?,
            current_sl: r.get("current_sl")?,
            target: r.get("target")?,
            is_active: r.get::<_, Option<bool>>("is_active")?.unwrap_or(true),
        })
    }
}

/// A create-or-update request: the key plus every field to set. A `None`
/// field is left as it is (the web's "present and not None" rule).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlUpsert {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub mode: String,
    pub side: Option<String>,
    pub entry_price: Option<f64>,
    pub quantity: Option<i64>,
    pub initial_sl: Option<f64>,
    pub trailing_enabled: Option<bool>,
    pub trailing_step: Option<f64>,
    pub highest_price: Option<f64>,
    pub lowest_price: Option<f64>,
    pub current_sl: Option<f64>,
    pub target: Option<f64>,
    pub is_active: Option<bool>,
}

/// One instrument on the scalping list.
#[derive(Debug, Clone, PartialEq)]
pub struct Tracked {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub mode: String,
}

impl Tracked {
    pub fn to_dict(&self) -> Value {
        json!({
            "symbol": self.symbol,
            "exchange": self.exchange,
            "product": self.product,
            "mode": self.mode,
        })
    }
}

const SL_COLUMNS: &str = "symbol, exchange, product, mode, side, entry_price, quantity, \
     initial_sl, trailing_enabled, trailing_step, highest_price, lowest_price, current_sl, \
     target, is_active";

#[derive(Clone)]
pub struct Store {
    db: Arc<SqliteDb>,
}

impl Store {
    pub fn new(db: Arc<SqliteDb>) -> Self {
        Self { db }
    }

    /// Create or update one leg's row and return it as stored.
    pub fn upsert_sl(&self, u: &SlUpsert) -> Result<SlState> {
        let mut conn = self.db.conn()?;
        let tx = conn.transaction()?;
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM scalping_sl_state
                 WHERE symbol = ?1 AND exchange = ?2 AND product = ?3 AND mode = ?4",
                params![u.symbol, u.exchange, u.product, u.mode],
                |r| r.get(0),
            )
            .optional()?;
        let id = match id {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO scalping_sl_state (symbol, exchange, product, mode)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![u.symbol, u.exchange, u.product, u.mode],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.execute(
            "UPDATE scalping_sl_state SET
                side = COALESCE(?2, side),
                entry_price = COALESCE(?3, entry_price),
                quantity = COALESCE(?4, quantity),
                initial_sl = COALESCE(?5, initial_sl),
                trailing_enabled = COALESCE(?6, trailing_enabled),
                trailing_step = COALESCE(?7, trailing_step),
                highest_price = COALESCE(?8, highest_price),
                lowest_price = COALESCE(?9, lowest_price),
                current_sl = COALESCE(?10, current_sl),
                target = COALESCE(?11, target),
                is_active = COALESCE(?12, is_active),
                updated_at = datetime('now')
             WHERE id = ?1",
            params![
                id,
                u.side,
                u.entry_price,
                u.quantity,
                u.initial_sl,
                u.trailing_enabled,
                u.trailing_step,
                u.highest_price,
                u.lowest_price,
                u.current_sl,
                u.target,
                u.is_active,
            ],
        )?;
        let row = tx.query_row(
            &format!("SELECT {} FROM scalping_sl_state WHERE id = ?1", SL_COLUMNS),
            [id],
            SlState::from_row,
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Active legs, optionally of one mode only.
    pub fn active_sl(&self, mode: Option<&str>) -> Result<Vec<SlState>> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM scalping_sl_state
             WHERE is_active = 1 AND (?1 IS NULL OR mode = ?1) ORDER BY id",
            SL_COLUMNS
        ))?;
        let rows = stmt
            .query_map([mode], SlState::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Remove one leg's row (scoped to `mode` when given). True when a row
    /// went.
    pub fn delete_sl(
        &self,
        symbol: &str,
        exchange: &str,
        product: &str,
        mode: Option<&str>,
    ) -> Result<bool> {
        let conn = self.db.conn()?;
        let n = conn.execute(
            "DELETE FROM scalping_sl_state
             WHERE symbol = ?1 AND exchange = ?2 AND product = ?3 AND (?4 IS NULL OR mode = ?4)",
            params![symbol, exchange, product, mode],
        )?;
        Ok(n > 0)
    }

    /// Write a trailed stop back. An update only: a leg cleared meanwhile is
    /// never brought back by a late write.
    pub fn update_trail(&self, s: &SlState) -> Result<()> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE scalping_sl_state
             SET current_sl = ?5, highest_price = ?6, lowest_price = ?7, updated_at = datetime('now')
             WHERE symbol = ?1 AND exchange = ?2 AND product = ?3 AND mode = ?4",
            params![
                s.symbol,
                s.exchange,
                s.product,
                s.mode,
                s.current_sl,
                s.highest_price,
                s.lowest_price
            ],
        )?;
        Ok(())
    }

    /// Put an instrument on the scalping list (idempotent).
    pub fn track(&self, symbol: &str, exchange: &str, product: &str, mode: &str) -> Result<()> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO scalping_tracked_symbol (symbol, exchange, product, mode)
             VALUES (?1, ?2, ?3, ?4)",
            params![symbol, exchange, product, mode],
        )?;
        Ok(())
    }

    pub fn tracked(&self, mode: Option<&str>) -> Result<Vec<Tracked>> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT symbol, exchange, product, mode FROM scalping_tracked_symbol
             WHERE (?1 IS NULL OR mode = ?1) ORDER BY id",
        )?;
        let rows = stmt
            .query_map([mode], |r| {
                Ok(Tracked {
                    symbol: r.get(0)?,
                    exchange: r.get(1)?,
                    product: r.get(2)?,
                    mode: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn clear_tracked(&self, mode: Option<&str>) -> Result<()> {
        let conn = self.db.conn()?;
        conn.execute(
            "DELETE FROM scalping_tracked_symbol WHERE (?1 IS NULL OR mode = ?1)",
            [mode],
        )?;
        Ok(())
    }
}
