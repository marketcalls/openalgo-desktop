//! `sandbox.db`: the sandbox's own SQLite file, fully isolated from the live
//! databases, with the web's table and column names
//! (`database/sandbox_db.py`).
//!
//! Money is stored as exact decimal text and computed with
//! `rust_decimal::Decimal`; timestamps are naive IST text. One connection
//! behind a mutex serialises every write, and every engine decision that
//! reads then writes runs inside one `BEGIN IMMEDIATE` transaction, so the
//! web's compare-and-set and conditional-UPDATE claims hold trivially and are
//! kept anyway as defence in depth.
//!
//! Schema changes ship as numbered, idempotent migrations in [`MIGRATIONS`].

use super::charges::Charges;
use super::types::{dec_from_db, Action, OrderStatus, PriceType, Product};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use rust_decimal::Decimal;
use std::path::Path;
use std::time::Duration;

/// One schema step: a batch of statements, or a function for a change
/// SQLite cannot write idempotently (`ADD COLUMN` has no `IF NOT EXISTS`).
pub enum Migration {
    Sql(&'static str),
    Run(fn(&Transaction) -> rusqlite::Result<()>),
}

/// Numbered migrations. Each runs once, in order, inside a transaction, and
/// is written to be safe to re-run (`IF NOT EXISTS`, `INSERT OR IGNORE`, a
/// column added only when it is missing).
pub const MIGRATIONS: &[(i64, Migration)] = &[
    (1, Migration::Sql(SCHEMA_V1)),
    (2, Migration::Run(add_charge_columns)),
];

/// Schema 2: what every fill was charged (web `upgrade/migrate_sandbox_charges.py`,
/// marketcalls/openalgo#2496). Money is exact decimal text, like every other
/// amount here. Rows that already exist read 0, which is what they paid: no
/// trade before this version was charged and no balance had a charge taken
/// from it, so nothing is backfilled.
pub const CHARGE_COLUMNS_BY_TABLE: &[(&str, &[&str])] = &[
    ("sandbox_trades", &super::charges::CHARGE_COLUMNS),
    ("sandbox_funds", &["total_charges", "today_charges"]),
    ("sandbox_daily_pnl", &["charges"]),
];

fn add_charge_columns(tx: &Transaction) -> rusqlite::Result<()> {
    for (table, columns) in CHARGE_COLUMNS_BY_TABLE {
        let existing: Vec<String> = {
            let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if existing.is_empty() {
            continue;
        }
        for column in columns.iter().filter(|c| !existing.iter().any(|e| e == *c)) {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} TEXT NOT NULL DEFAULT '0'"
            ))?;
        }
    }
    Ok(())
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS sandbox_orders (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  orderid TEXT NOT NULL UNIQUE,
  user_id TEXT NOT NULL,
  strategy TEXT,
  symbol TEXT NOT NULL,
  exchange TEXT NOT NULL,
  action TEXT NOT NULL CHECK (action IN ('BUY','SELL')),
  quantity INTEGER NOT NULL,
  price TEXT,
  trigger_price TEXT,
  price_type TEXT NOT NULL CHECK (price_type IN ('MARKET','LIMIT','SL','SL-M')),
  product TEXT NOT NULL CHECK (product IN ('CNC','NRML','MIS')),
  order_status TEXT NOT NULL DEFAULT 'open'
    CHECK (order_status IN ('open','trigger pending','complete','cancelled','rejected')),
  average_price TEXT,
  filled_quantity INTEGER NOT NULL DEFAULT 0,
  pending_quantity INTEGER NOT NULL,
  rejection_reason TEXT,
  margin_blocked TEXT NOT NULL DEFAULT '0',
  gtt_leg_id INTEGER,
  order_timestamp TEXT NOT NULL,
  update_timestamp TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sandbox_orders_gtt_leg
  ON sandbox_orders(gtt_leg_id) WHERE gtt_leg_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_sandbox_user_status ON sandbox_orders(user_id, order_status);
CREATE INDEX IF NOT EXISTS idx_sandbox_symbol_exchange ON sandbox_orders(symbol, exchange);
CREATE INDEX IF NOT EXISTS idx_sandbox_orders_status ON sandbox_orders(order_status);
CREATE INDEX IF NOT EXISTS idx_sandbox_orders_ts ON sandbox_orders(user_id, order_timestamp);

CREATE TABLE IF NOT EXISTS sandbox_trades (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  tradeid TEXT NOT NULL UNIQUE,
  orderid TEXT NOT NULL,
  user_id TEXT NOT NULL,
  symbol TEXT NOT NULL,
  exchange TEXT NOT NULL,
  action TEXT NOT NULL,
  quantity INTEGER NOT NULL,
  price TEXT NOT NULL,
  product TEXT NOT NULL,
  strategy TEXT,
  trade_timestamp TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_user_symbol ON sandbox_trades(user_id, symbol);
CREATE INDEX IF NOT EXISTS idx_orderid ON sandbox_trades(orderid);
CREATE INDEX IF NOT EXISTS idx_trades_ts ON sandbox_trades(user_id, trade_timestamp);

CREATE TABLE IF NOT EXISTS sandbox_positions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id TEXT NOT NULL,
  symbol TEXT NOT NULL,
  exchange TEXT NOT NULL,
  product TEXT NOT NULL,
  quantity INTEGER NOT NULL,
  average_price TEXT NOT NULL,
  ltp TEXT,
  pnl TEXT NOT NULL DEFAULT '0',
  pnl_percent TEXT NOT NULL DEFAULT '0',
  accumulated_realized_pnl TEXT NOT NULL DEFAULT '0',
  today_realized_pnl TEXT NOT NULL DEFAULT '0',
  margin_blocked TEXT NOT NULL DEFAULT '0',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE (user_id, symbol, exchange, product)
);
CREATE INDEX IF NOT EXISTS idx_user_product ON sandbox_positions(user_id, product);

CREATE TABLE IF NOT EXISTS sandbox_holdings (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id TEXT NOT NULL,
  symbol TEXT NOT NULL,
  exchange TEXT NOT NULL,
  quantity INTEGER NOT NULL,
  average_price TEXT NOT NULL,
  ltp TEXT,
  pnl TEXT NOT NULL DEFAULT '0',
  pnl_percent TEXT NOT NULL DEFAULT '0',
  settlement_date TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE (user_id, symbol, exchange)
);

CREATE TABLE IF NOT EXISTS sandbox_funds (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id TEXT NOT NULL UNIQUE,
  total_capital TEXT NOT NULL DEFAULT '10000000',
  available_balance TEXT NOT NULL DEFAULT '10000000',
  used_margin TEXT NOT NULL DEFAULT '0',
  realized_pnl TEXT NOT NULL DEFAULT '0',
  today_realized_pnl TEXT NOT NULL DEFAULT '0',
  unrealized_pnl TEXT NOT NULL DEFAULT '0',
  total_pnl TEXT NOT NULL DEFAULT '0',
  last_reset_date TEXT NOT NULL,
  reset_count INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sandbox_daily_pnl (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id TEXT NOT NULL,
  date TEXT NOT NULL,
  realized_pnl TEXT NOT NULL DEFAULT '0',
  positions_unrealized_pnl TEXT NOT NULL DEFAULT '0',
  holdings_unrealized_pnl TEXT NOT NULL DEFAULT '0',
  total_mtm TEXT NOT NULL DEFAULT '0',
  available_balance TEXT NOT NULL DEFAULT '0',
  used_margin TEXT NOT NULL DEFAULT '0',
  portfolio_value TEXT NOT NULL DEFAULT '0',
  created_at TEXT NOT NULL,
  UNIQUE (user_id, date)
);
CREATE INDEX IF NOT EXISTS idx_user_date ON sandbox_daily_pnl(user_id, date);

CREATE TABLE IF NOT EXISTS sandbox_config (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  config_key TEXT NOT NULL UNIQUE,
  config_value TEXT NOT NULL,
  description TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sandbox_gtt (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  gtt_id TEXT NOT NULL UNIQUE,
  user_id TEXT NOT NULL,
  strategy TEXT,
  trigger_type TEXT NOT NULL CHECK (trigger_type IN ('single','two-leg')),
  symbol TEXT NOT NULL,
  exchange TEXT NOT NULL,
  last_price TEXT NOT NULL DEFAULT '0',
  gtt_status TEXT NOT NULL DEFAULT 'active'
    CHECK (gtt_status IN ('active','triggered','cancelled','expired','rejected')),
  margin_blocked TEXT NOT NULL DEFAULT '0',
  expires_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_gtt_user_status ON sandbox_gtt(user_id, gtt_status);
CREATE INDEX IF NOT EXISTS idx_gtt_symbol_exchange ON sandbox_gtt(symbol, exchange);

CREATE TABLE IF NOT EXISTS sandbox_gtt_legs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  gtt_id TEXT NOT NULL REFERENCES sandbox_gtt(gtt_id) ON DELETE CASCADE,
  leg_number INTEGER NOT NULL,
  trigger_price TEXT NOT NULL,
  trigger_direction TEXT NOT NULL DEFAULT 'below' CHECK (trigger_direction IN ('below','above')),
  action TEXT NOT NULL,
  quantity INTEGER NOT NULL,
  price TEXT NOT NULL DEFAULT '0',
  pricetype TEXT NOT NULL DEFAULT 'LIMIT',
  product TEXT NOT NULL,
  leg_status TEXT NOT NULL DEFAULT 'pending'
    CHECK (leg_status IN ('pending','triggering','triggered','cancelled')),
  triggered_order_id TEXT,
  leg_margin TEXT NOT NULL DEFAULT '0',
  claimed_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_gtt_leg_status_claimed ON sandbox_gtt_legs(leg_status, claimed_at);
CREATE INDEX IF NOT EXISTS idx_gtt_leg_gtt ON sandbox_gtt_legs(gtt_id);
"#;

/// The sandbox database.
pub struct SandboxDb {
    conn: Mutex<Connection>,
}

impl SandboxDb {
    /// Open (creating if needed) `sandbox.db` at `path` and migrate it.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        // The per-store durability policy lives with the other stores'.
        conn.execute_batch(crate::db::sqlite::Durability::SANDBOX.pragma())?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    /// A private in-memory database (tests).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sandbox_schema_migrations (
               version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )?;
        for (version, step) in MIGRATIONS {
            let done: Option<i64> = conn
                .query_row(
                    "SELECT version FROM sandbox_schema_migrations WHERE version = ?1",
                    [version],
                    |r| r.get(0),
                )
                .optional()?;
            if done.is_some() {
                continue;
            }
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            match step {
                Migration::Sql(sql) => tx.execute_batch(sql)?,
                Migration::Run(run) => run(&tx)?,
            }
            tx.execute(
                "INSERT INTO sandbox_schema_migrations (version) VALUES (?1)",
                [version],
            )?;
            tx.commit()?;
        }
        // Seed config defaults; never overwrite a value the trader changed.
        let now = chrono::Utc::now()
            .with_timezone(&super::clock::IST)
            .naive_local();
        let now = super::clock::ts(now);
        let seed = "INSERT OR IGNORE INTO sandbox_config (config_key, config_value, description, updated_at)
                    VALUES (?1, ?2, ?3, ?4)";
        for (k, v, d) in super::config::DEFAULTS {
            conn.execute(seed, rusqlite::params![k, v, d, now])?;
        }
        // The brokerage plan and the charges switch, from the module that
        // applies them, so the seed cannot drift from the calculation.
        for (k, v, d) in super::charges::default_config_rows() {
            conn.execute(seed, rusqlite::params![k, v, d, now])?;
        }
        Ok(())
    }

    /// Run `f` inside one `BEGIN IMMEDIATE` transaction: committed when `f`
    /// returns `Ok`, rolled back otherwise. Never call across an `.await`.
    pub fn with_tx<R, E>(&self, f: impl FnOnce(&Transaction) -> Result<R, E>) -> Result<R, E>
    where
        E: From<rusqlite::Error>,
    {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Read-only access (no transaction).
    pub fn with_conn<R, E>(&self, f: impl FnOnce(&Connection) -> Result<R, E>) -> Result<R, E>
    where
        E: From<rusqlite::Error>,
    {
        let conn = self.conn.lock();
        f(&conn)
    }

    /// Applied migration versions (tests and diagnostics).
    pub fn applied_migrations(&self) -> rusqlite::Result<Vec<i64>> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare("SELECT version FROM sandbox_schema_migrations ORDER BY version")?;
        let v = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

pub(crate) fn dec_col(row: &Row, name: &str) -> rusqlite::Result<Decimal> {
    let v: Option<String> = row.get(name)?;
    Ok(v.map(|s| dec_from_db(&s)).unwrap_or(Decimal::ZERO))
}

pub(crate) fn opt_dec_col(row: &Row, name: &str) -> rusqlite::Result<Option<Decimal>> {
    let v: Option<String> = row.get(name)?;
    Ok(v.map(|s| dec_from_db(&s)))
}

/// `(quantity, price, trigger_price, price_type, action)` of an order.
pub type OrderTerms = (i64, Option<Decimal>, Option<Decimal>, PriceType, Action);

/// One `sandbox_orders` row.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRow {
    pub id: i64,
    pub orderid: String,
    pub user_id: String,
    pub strategy: Option<String>,
    pub symbol: String,
    pub exchange: String,
    pub action: Action,
    pub quantity: i64,
    pub price: Option<Decimal>,
    pub trigger_price: Option<Decimal>,
    pub price_type: PriceType,
    pub product: Product,
    pub order_status: OrderStatus,
    pub average_price: Option<Decimal>,
    pub filled_quantity: i64,
    pub pending_quantity: i64,
    pub rejection_reason: Option<String>,
    pub margin_blocked: Decimal,
    pub gtt_leg_id: Option<i64>,
    pub order_timestamp: String,
    pub update_timestamp: String,
}

impl OrderRow {
    pub const COLUMNS: &'static str = "id, orderid, user_id, strategy, symbol, exchange, action, quantity, price, \
        trigger_price, price_type, product, order_status, average_price, filled_quantity, \
        pending_quantity, rejection_reason, margin_blocked, gtt_leg_id, order_timestamp, update_timestamp";

    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let action: String = r.get("action")?;
        let price_type: String = r.get("price_type")?;
        let product: String = r.get("product")?;
        let status: String = r.get("order_status")?;
        Ok(Self {
            id: r.get("id")?,
            orderid: r.get("orderid")?,
            user_id: r.get("user_id")?,
            strategy: r.get("strategy")?,
            symbol: r.get("symbol")?,
            exchange: r.get("exchange")?,
            action: Action::parse(&action).unwrap_or(Action::Buy),
            quantity: r.get("quantity")?,
            price: opt_dec_col(r, "price")?,
            trigger_price: opt_dec_col(r, "trigger_price")?,
            price_type: PriceType::parse(&price_type).unwrap_or(PriceType::Market),
            product: Product::parse(&product).unwrap_or(Product::Mis),
            order_status: OrderStatus::parse(&status).unwrap_or(OrderStatus::Rejected),
            average_price: opt_dec_col(r, "average_price")?,
            filled_quantity: r.get("filled_quantity")?,
            pending_quantity: r.get("pending_quantity")?,
            rejection_reason: r.get("rejection_reason")?,
            margin_blocked: dec_col(r, "margin_blocked")?,
            gtt_leg_id: r.get("gtt_leg_id")?,
            order_timestamp: r.get("order_timestamp")?,
            update_timestamp: r.get("update_timestamp")?,
        })
    }

    /// What a fill decision was made against; a change means a modify landed.
    pub fn terms(&self) -> OrderTerms {
        (
            self.quantity,
            self.price,
            self.trigger_price,
            self.price_type,
            self.action,
        )
    }
}

/// One `sandbox_trades` row.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeRow {
    pub id: i64,
    pub tradeid: String,
    pub orderid: String,
    pub user_id: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub price: Decimal,
    pub product: String,
    pub strategy: Option<String>,
    pub trade_timestamp: String,
    /// What the fill was charged (zero for trades before schema 2).
    pub charges: Charges,
}

impl TradeRow {
    pub const COLUMNS: &'static str =
        "id, tradeid, orderid, user_id, symbol, exchange, action, quantity, price, \
        product, strategy, trade_timestamp, brokerage, stt, exchange_charges, sebi_charges, \
        stamp_duty, gst";

    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            tradeid: r.get("tradeid")?,
            orderid: r.get("orderid")?,
            user_id: r.get("user_id")?,
            symbol: r.get("symbol")?,
            exchange: r.get("exchange")?,
            action: r.get("action")?,
            quantity: r.get("quantity")?,
            price: dec_col(r, "price")?,
            product: r.get("product")?,
            strategy: r.get("strategy")?,
            trade_timestamp: r.get("trade_timestamp")?,
            charges: Charges {
                brokerage: dec_col(r, "brokerage")?,
                stt: dec_col(r, "stt")?,
                exchange_charges: dec_col(r, "exchange_charges")?,
                sebi_charges: dec_col(r, "sebi_charges")?,
                stamp_duty: dec_col(r, "stamp_duty")?,
                gst: dec_col(r, "gst")?,
            },
        })
    }
}

/// One `sandbox_positions` row.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionRow {
    pub id: i64,
    pub user_id: String,
    pub symbol: String,
    pub exchange: String,
    pub product: Product,
    pub quantity: i64,
    pub average_price: Decimal,
    pub ltp: Option<Decimal>,
    pub pnl: Decimal,
    pub pnl_percent: Decimal,
    pub accumulated_realized_pnl: Decimal,
    pub today_realized_pnl: Decimal,
    pub margin_blocked: Decimal,
    pub created_at: String,
    pub updated_at: String,
}

impl PositionRow {
    pub const COLUMNS: &'static str = "id, user_id, symbol, exchange, product, quantity, average_price, ltp, pnl, \
        pnl_percent, accumulated_realized_pnl, today_realized_pnl, margin_blocked, created_at, updated_at";

    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let product: String = r.get("product")?;
        Ok(Self {
            id: r.get("id")?,
            user_id: r.get("user_id")?,
            symbol: r.get("symbol")?,
            exchange: r.get("exchange")?,
            product: Product::parse(&product).unwrap_or(Product::Mis),
            quantity: r.get("quantity")?,
            average_price: dec_col(r, "average_price")?,
            ltp: opt_dec_col(r, "ltp")?,
            pnl: dec_col(r, "pnl")?,
            pnl_percent: dec_col(r, "pnl_percent")?,
            accumulated_realized_pnl: dec_col(r, "accumulated_realized_pnl")?,
            today_realized_pnl: dec_col(r, "today_realized_pnl")?,
            margin_blocked: dec_col(r, "margin_blocked")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }
}

/// One `sandbox_holdings` row.
#[derive(Debug, Clone, PartialEq)]
pub struct HoldingRow {
    pub id: i64,
    pub user_id: String,
    pub symbol: String,
    pub exchange: String,
    pub quantity: i64,
    pub average_price: Decimal,
    pub ltp: Option<Decimal>,
    pub pnl: Decimal,
    pub pnl_percent: Decimal,
    pub settlement_date: String,
    pub created_at: String,
    pub updated_at: String,
}

impl HoldingRow {
    pub const COLUMNS: &'static str =
        "id, user_id, symbol, exchange, quantity, average_price, ltp, pnl, pnl_percent, \
        settlement_date, created_at, updated_at";

    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            user_id: r.get("user_id")?,
            symbol: r.get("symbol")?,
            exchange: r.get("exchange")?,
            quantity: r.get("quantity")?,
            average_price: dec_col(r, "average_price")?,
            ltp: opt_dec_col(r, "ltp")?,
            pnl: dec_col(r, "pnl")?,
            pnl_percent: dec_col(r, "pnl_percent")?,
            settlement_date: r.get("settlement_date")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }
}

/// One `sandbox_daily_pnl` row.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyPnlRow {
    pub user_id: String,
    pub date: String,
    pub realized_pnl: Decimal,
    pub positions_unrealized_pnl: Decimal,
    pub holdings_unrealized_pnl: Decimal,
    pub total_mtm: Decimal,
    /// The day's charges; `total_mtm` is gross, the day's net is the two
    /// apart.
    pub charges: Decimal,
    pub available_balance: Decimal,
    pub used_margin: Decimal,
    pub portfolio_value: Decimal,
}

impl DailyPnlRow {
    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            user_id: r.get("user_id")?,
            date: r.get("date")?,
            realized_pnl: dec_col(r, "realized_pnl")?,
            positions_unrealized_pnl: dec_col(r, "positions_unrealized_pnl")?,
            holdings_unrealized_pnl: dec_col(r, "holdings_unrealized_pnl")?,
            total_mtm: dec_col(r, "total_mtm")?,
            charges: dec_col(r, "charges")?,
            available_balance: dec_col(r, "available_balance")?,
            used_margin: dec_col(r, "used_margin")?,
            portfolio_value: dec_col(r, "portfolio_value")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_recorded_and_rerun_safely() {
        let db = SandboxDb::open_in_memory().unwrap();
        assert_eq!(db.applied_migrations().unwrap(), vec![1, 2]);
        db.migrate().unwrap();
        assert_eq!(db.applied_migrations().unwrap(), vec![1, 2]);
        let n: i64 = db
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM sandbox_config", [], |r| r.get(0)))
            .unwrap();
        // The web's 22 settings, the charges switch and two per segment.
        assert_eq!(n, 22 + 1 + 2 * 10);
    }

    fn columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
        rows.map(Result::unwrap).collect()
    }

    fn text(conn: &Connection, sql: &str) -> String {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// A schema-1 `sandbox.db` from an earlier version, in use: a trade, a
    /// funds row, a day's P&L, and a brokerage value the trader saved
    /// before (as the web's test does, a plan key already present).
    fn schema_one_database(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(
            "CREATE TABLE sandbox_schema_migrations (
               version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT (datetime('now')));
             INSERT INTO sandbox_schema_migrations (version) VALUES (1);
             INSERT INTO sandbox_trades (tradeid, orderid, user_id, symbol, exchange, action,
               quantity, price, product, strategy, trade_timestamp)
             VALUES ('TRADE-1', '1', 'u', 'SBIN', 'NSE', 'BUY', 10, '800', 'CNC', 's',
               '2026-11-20 10:00:00.000000');
             INSERT INTO sandbox_funds (user_id, total_capital, available_balance, used_margin,
               realized_pnl, today_realized_pnl, unrealized_pnl, total_pnl, last_reset_date,
               reset_count, created_at, updated_at)
             VALUES ('u', '10000000', '9992000', '8000', '150', '0', '0', '150',
               '2026-11-01 09:00:00', 0, '2026-11-01 09:00:00', '2026-11-20 10:00:00');
             INSERT INTO sandbox_daily_pnl (user_id, date, realized_pnl, total_mtm, created_at)
             VALUES ('u', '2026-11-19', '150', '150', '2026-11-19 23:59:00');
             INSERT INTO sandbox_config (config_key, config_value, description, updated_at)
             VALUES ('brokerage_fno_options_flat', '15', 'saved', '2026-11-20 10:00:00'),
                    ('equity_mis_leverage', '3', 'saved', '2026-11-20 10:00:00');",
        )
        .unwrap();
    }

    /// Web test_migrate_sandbox_charges.py: every column added, the rows
    /// already there read as uncharged, the default plan seeded without
    /// overwriting a value the trader saved or any other setting, and a
    /// second open changes nothing.
    #[test]
    fn schema_two_upgrades_a_database_in_use_and_overwrites_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sandbox.db");
        schema_one_database(&path);
        let db = SandboxDb::open(&path).unwrap();
        assert_eq!(db.applied_migrations().unwrap(), vec![1, 2]);
        db.with_conn(|c| -> rusqlite::Result<()> {
            for (table, added) in CHARGE_COLUMNS_BY_TABLE {
                let have = columns(c, table);
                for col in added.iter() {
                    assert!(have.iter().any(|h| h == col), "{table}.{col}");
                }
            }
            let trade: TradeRow = c.query_row(
                &format!("SELECT {} FROM sandbox_trades", TradeRow::COLUMNS),
                [],
                TradeRow::from_row,
            )?;
            assert_eq!(trade.charges, Charges::default());
            assert_eq!(trade.price, Decimal::from(800));
            assert_eq!(text(c, "SELECT total_charges FROM sandbox_trades"), "0");
            assert_eq!(
                text(c, "SELECT total_charges || '/' || today_charges || '/' || available_balance FROM sandbox_funds"),
                "0/0/9992000"
            );
            assert_eq!(text(c, "SELECT charges FROM sandbox_daily_pnl"), "0");
            let get = |k: &str| {
                text(
                    c,
                    &format!("SELECT config_value FROM sandbox_config WHERE config_key = '{k}'"),
                )
            };
            assert_eq!(get("brokerage_fno_options_flat"), "15");
            assert_eq!(get("equity_mis_leverage"), "3");
            assert_eq!(get("charges_enabled"), "true");
            assert_eq!(get("brokerage_equity_delivery_percent"), "2.5");
            Ok(())
        })
        .unwrap();
        drop(db);
        // Opened again: nothing new to apply, nothing changed.
        let db = SandboxDb::open(&path).unwrap();
        assert_eq!(db.applied_migrations().unwrap(), vec![1, 2]);
        let n: i64 = db
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM sandbox_config", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 22 + 1 + 2 * 10);
    }

    /// Web test_migrate_sandbox_charges.py::test_a_database_with_only_some_tables_gets_only_their_columns:
    /// a step that finds a column already there (a run cut short after
    /// one `ALTER`) adds only the missing ones.
    #[test]
    fn a_column_already_added_is_not_added_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sandbox.db");
        schema_one_database(&path);
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "ALTER TABLE sandbox_funds ADD COLUMN total_charges TEXT NOT NULL DEFAULT '0'",
            )
            .unwrap();
        let db = SandboxDb::open(&path).unwrap();
        assert_eq!(db.applied_migrations().unwrap(), vec![1, 2]);
        let have = db
            .with_conn(|c| Ok::<_, rusqlite::Error>(columns(c, "sandbox_funds")))
            .unwrap();
        assert_eq!(have.iter().filter(|c| *c == "total_charges").count(), 1);
        assert!(have.iter().any(|c| c == "today_charges"));
    }

    #[test]
    fn status_check_constraint_rejects_unknown_status() {
        let db = SandboxDb::open_in_memory().unwrap();
        let r: rusqlite::Result<()> = db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO sandbox_orders (orderid, user_id, symbol, exchange, action, quantity,
                 price_type, product, order_status, pending_quantity, order_timestamp, update_timestamp)
                 VALUES ('1','u','S','NSE','BUY',1,'MARKET','MIS','trigger_pending',1,'t','t')",
                [],
            )?;
            Ok(())
        });
        assert!(r.is_err(), "'trigger_pending' (underscore) must be refused");
    }

    #[test]
    fn a_user_customised_config_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sandbox.db");
        {
            let db = SandboxDb::open(&path).unwrap();
            db.with_conn(|c| {
                c.execute(
                    "UPDATE sandbox_config SET config_value='3' WHERE config_key='equity_mis_leverage'",
                    [],
                )
            })
            .unwrap();
        }
        let db = SandboxDb::open(&path).unwrap();
        let v: String = db
            .with_conn(|c| {
                c.query_row(
                    "SELECT config_value FROM sandbox_config WHERE config_key='equity_mis_leverage'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(v, "3");
    }
}
