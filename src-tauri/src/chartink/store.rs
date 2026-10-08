//! Persistence for Chartink strategies (web `database/chartink_db.py`):
//! `chartink_strategies` and `chartink_symbol_mappings` in the main SQLite
//! database, with the web's column names.
//!
//! The legacy schema (migrations 007 and 008) created both tables with an
//! older shape (`enabled`, `product`, `symbol`). Migration `075_chartink`
//! adds the web's columns and backfills them from the old ones: `is_active`
//! from `enabled`, `chartink_symbol` from `symbol`, `product_type` from the
//! strategy's `product`, `user_id` from the one account. The old columns stay
//! (written alongside) so nothing reading them breaks.
//!
//! `updated_at` is NOT NULL in the legacy table; the web leaves it null until
//! the first change, so a row whose `updated_at` equals its `created_at` is
//! reported with `updated_at: null`.

use crate::db::sqlite::SqliteDb;
use crate::error::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};
use std::sync::Arc;

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|n| n == column))
}

fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<bool> {
    if column_exists(conn, table, column)? {
        return Ok(false);
    }
    conn.execute_batch(&format!(
        "ALTER TABLE {} ADD COLUMN {} {}",
        table, column, decl
    ))?;
    Ok(true)
}

/// Migration `075_chartink`. Idempotent: creates the tables when missing,
/// adds each web column only when missing, and backfills only rows the new
/// column does not yet describe.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chartink_strategies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            webhook_id TEXT NOT NULL UNIQUE,
            scan_url TEXT,
            product TEXT NOT NULL DEFAULT 'MIS',
            quantity INTEGER NOT NULL DEFAULT 1,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS chartink_symbol_mappings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            strategy_id INTEGER NOT NULL REFERENCES chartink_strategies(id) ON DELETE CASCADE,
            exchange TEXT NOT NULL,
            symbol TEXT NOT NULL,
            quantity INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    add_column(conn, "chartink_strategies", "user_id", "TEXT")?;
    let new_active = add_column(conn, "chartink_strategies", "is_active", "INTEGER")?;
    add_column(conn, "chartink_strategies", "is_intraday", "INTEGER")?;
    add_column(conn, "chartink_strategies", "start_time", "TEXT")?;
    add_column(conn, "chartink_strategies", "end_time", "TEXT")?;
    add_column(conn, "chartink_strategies", "squareoff_time", "TEXT")?;
    add_column(conn, "chartink_symbol_mappings", "chartink_symbol", "TEXT")?;
    add_column(conn, "chartink_symbol_mappings", "product_type", "TEXT")?;
    add_column(conn, "chartink_symbol_mappings", "updated_at", "TEXT")?;

    // Backfill from the existing data, never over a value already set.
    if new_active || column_exists(conn, "chartink_strategies", "enabled")? {
        conn.execute(
            "UPDATE chartink_strategies SET is_active = COALESCE(enabled, 1) WHERE is_active IS NULL",
            [],
        )?;
    }
    // A legacy strategy had no trading window: positional.
    conn.execute(
        "UPDATE chartink_strategies SET is_intraday = 0
         WHERE is_intraday IS NULL AND start_time IS NULL AND squareoff_time IS NULL",
        [],
    )?;
    conn.execute(
        "UPDATE chartink_strategies
         SET user_id = (SELECT username FROM users ORDER BY id LIMIT 1)
         WHERE user_id IS NULL",
        [],
    )?;
    conn.execute(
        "UPDATE chartink_symbol_mappings SET chartink_symbol = symbol WHERE chartink_symbol IS NULL",
        [],
    )?;
    conn.execute(
        "UPDATE chartink_symbol_mappings
         SET product_type = COALESCE(
            (SELECT s.product FROM chartink_strategies s WHERE s.id = chartink_symbol_mappings.strategy_id),
            'MIS')
         WHERE product_type IS NULL",
        [],
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_chartink_mappings_strategy
         ON chartink_symbol_mappings(strategy_id)",
    )?;
    Ok(())
}

/// One strategy (the web's model).
#[derive(Debug, Clone, PartialEq)]
pub struct Strategy {
    pub id: i64,
    pub name: String,
    pub webhook_id: String,
    pub user_id: String,
    pub is_active: bool,
    pub is_intraday: bool,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub squareoff_time: Option<String>,
    pub created_at: String,
    pub updated_at: Option<String>,
}

/// SQLite's `YYYY-MM-DD HH:MM:SS` as Python's `isoformat()`.
fn iso(ts: &str) -> String {
    ts.replacen(' ', "T", 1)
}

impl Strategy {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let created: String = r
            .get::<_, Option<String>>("created_at")?
            .unwrap_or_default();
        let updated: Option<String> = r.get("updated_at")?;
        Ok(Self {
            id: r.get("id")?,
            name: r.get("name")?,
            webhook_id: r.get("webhook_id")?,
            user_id: r.get::<_, Option<String>>("user_id")?.unwrap_or_default(),
            is_active: r.get::<_, Option<bool>>("is_active")?.unwrap_or(true),
            is_intraday: r.get::<_, Option<bool>>("is_intraday")?.unwrap_or(true),
            start_time: r.get("start_time")?,
            end_time: r.get("end_time")?,
            squareoff_time: r.get("squareoff_time")?,
            updated_at: updated.filter(|u| *u != created),
            created_at: created,
        })
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "webhook_id": self.webhook_id,
            "is_active": self.is_active,
            "is_intraday": self.is_intraday,
            "start_time": self.start_time,
            "end_time": self.end_time,
            "squareoff_time": self.squareoff_time,
            "created_at": if self.created_at.is_empty() { Value::Null } else { json!(iso(&self.created_at)) },
            "updated_at": self.updated_at.as_deref().map(iso),
        })
    }
}

/// One symbol mapping.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    pub id: i64,
    pub strategy_id: i64,
    pub chartink_symbol: String,
    pub exchange: String,
    pub quantity: i64,
    pub product_type: String,
    pub created_at: Option<String>,
}

impl Mapping {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            strategy_id: r.get("strategy_id")?,
            chartink_symbol: r
                .get::<_, Option<String>>("chartink_symbol")?
                .unwrap_or_default(),
            exchange: r.get("exchange")?,
            quantity: r.get::<_, Option<i64>>("quantity")?.unwrap_or(0),
            product_type: r
                .get::<_, Option<String>>("product_type")?
                .unwrap_or_default(),
            created_at: r.get("created_at")?,
        })
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "id": self.id,
            "chartink_symbol": self.chartink_symbol,
            "exchange": self.exchange,
            "quantity": self.quantity,
            "product_type": self.product_type,
            "created_at": self.created_at.as_deref().map(iso),
        })
    }
}

/// A new mapping.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMapping {
    pub chartink_symbol: String,
    pub exchange: String,
    pub quantity: i64,
    pub product_type: String,
}

/// A new strategy.
#[derive(Debug, Clone, PartialEq)]
pub struct NewStrategy {
    pub name: String,
    pub webhook_id: String,
    pub user_id: String,
    pub is_intraday: bool,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub squareoff_time: Option<String>,
}

const STRATEGY_COLUMNS: &str = "id, name, webhook_id, user_id, is_active, is_intraday, \
     start_time, end_time, squareoff_time, created_at, updated_at";
const MAPPING_COLUMNS: &str =
    "id, strategy_id, chartink_symbol, exchange, quantity, product_type, created_at";

/// How many leading characters of a webhook id locate its row; the rest is
/// compared in constant time.
pub const LOCATOR_LEN: usize = 8;

fn stamp(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[derive(Clone)]
pub struct Store {
    db: Arc<SqliteDb>,
}

impl Store {
    pub fn new(db: Arc<SqliteDb>) -> Self {
        Self { db }
    }

    pub fn create(&self, s: &NewStrategy, now: DateTime<Utc>) -> Result<Strategy> {
        let conn = self.db.conn()?;
        let ts = stamp(now);
        conn.execute(
            "INSERT INTO chartink_strategies
               (name, webhook_id, user_id, is_active, enabled, is_intraday,
                start_time, end_time, squareoff_time, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, 1, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                s.name,
                s.webhook_id,
                s.user_id,
                s.is_intraday,
                s.start_time,
                s.end_time,
                s.squareoff_time,
                ts
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.get(id)?
            .ok_or_else(|| crate::error::AppError::NotFound("chartink strategy".into()))
    }

    pub fn get(&self, id: i64) -> Result<Option<Strategy>> {
        let conn = self.db.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM chartink_strategies WHERE id = ?1",
                    STRATEGY_COLUMNS
                ),
                [id],
                Strategy::from_row,
            )
            .optional()?)
    }

    pub fn for_user(&self, user: &str) -> Result<Vec<Strategy>> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM chartink_strategies WHERE user_id = ?1 ORDER BY id",
            STRATEGY_COLUMNS
        ))?;
        let rows = stmt
            .query_map([user], Strategy::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn all(&self) -> Result<Vec<Strategy>> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM chartink_strategies ORDER BY id",
            STRATEGY_COLUMNS
        ))?;
        let rows = stmt
            .query_map([], Strategy::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Strategies whose webhook id starts with `locator` (normally one).
    pub fn by_locator(&self, locator: &str) -> Result<Vec<Strategy>> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM chartink_strategies WHERE substr(webhook_id, 1, ?2) = ?1 ORDER BY id",
            STRATEGY_COLUMNS
        ))?;
        let rows = stmt
            .query_map(params![locator, LOCATOR_LEN as i64], Strategy::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Flip `is_active`; the updated row.
    pub fn toggle(&self, id: i64, now: DateTime<Utc>) -> Result<Option<Strategy>> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE chartink_strategies
             SET is_active = CASE WHEN COALESCE(is_active, 1) = 1 THEN 0 ELSE 1 END,
                 enabled = CASE WHEN COALESCE(is_active, 1) = 1 THEN 0 ELSE 1 END,
                 updated_at = ?2
             WHERE id = ?1",
            params![id, stamp(now)],
        )?;
        drop(conn);
        self.get(id)
    }

    /// Delete a strategy and its mappings. True when it existed.
    pub fn delete(&self, id: i64) -> Result<bool> {
        let mut conn = self.db.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM chartink_symbol_mappings WHERE strategy_id = ?1",
            [id],
        )?;
        let n = tx.execute("DELETE FROM chartink_strategies WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn add_mappings(
        &self,
        strategy_id: i64,
        rows: &[NewMapping],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut conn = self.db.conn()?;
        let tx = conn.transaction()?;
        let ts = stamp(now);
        for m in rows {
            tx.execute(
                "INSERT INTO chartink_symbol_mappings
                   (strategy_id, symbol, chartink_symbol, exchange, quantity, product_type, created_at)
                 VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6)",
                params![strategy_id, m.chartink_symbol, m.exchange, m.quantity, m.product_type, ts],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mappings(&self, strategy_id: i64) -> Result<Vec<Mapping>> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM chartink_symbol_mappings WHERE strategy_id = ?1 ORDER BY id",
            MAPPING_COLUMNS
        ))?;
        let rows = stmt
            .query_map([strategy_id], Mapping::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Delete one mapping of one strategy. True when it existed.
    pub fn delete_mapping(&self, strategy_id: i64, mapping_id: i64) -> Result<bool> {
        let conn = self.db.conn()?;
        let n = conn.execute(
            "DELETE FROM chartink_symbol_mappings WHERE id = ?1 AND strategy_id = ?2",
            params![mapping_id, strategy_id],
        )?;
        Ok(n > 0)
    }
}
