//! Action Center store (web `database/action_center_db.py`): orders an API
//! key in Semi-Auto mode queued for the trader's approval.
//!
//! Every status transition is one conditional `UPDATE` whose `WHERE` clause
//! carries the check (compare-and-set), so of two requests racing for one
//! order (a double click, two windows, an approve racing a reject) exactly
//! one wins. The execution claim works the same way: `broker_status` moves
//! from empty to `submitting` for at most one caller, before anything is
//! sent to the broker.

use crate::error::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};

/// `broker_status` while an approved order is on its way to the broker.
/// An order left in it by a crash is never resent automatically.
pub const SUBMITTING: &str = "submitting";

/// `broker_status` of an approved smart order that needed no order.
pub const NO_ACTION: &str = "no_action";

/// Migration `060_action_center_pending_orders`.
///
/// Builds the web's `pending_orders` table. Older desktop builds created a
/// placeholder table of the same name with different columns that nothing
/// wrote to; its rows (if any) are carried over as queued `placeorder`
/// requests rather than dropped.
pub fn migrate(conn: &Connection) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'pending_orders')",
        [],
        |r| r.get(0),
    )?;
    let has_api_type =
        exists && super::migrations::column_exists(conn, "pending_orders", "api_type")?;
    if exists && !has_api_type {
        conn.execute_batch("ALTER TABLE pending_orders RENAME TO pending_orders_legacy")?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pending_orders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id TEXT NOT NULL,
            api_type TEXT NOT NULL,
            order_data TEXT NOT NULL,
            created_at TEXT NOT NULL,
            created_at_ist TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            approved_at TEXT,
            approved_at_ist TEXT,
            approved_by TEXT,
            rejected_at TEXT,
            rejected_at_ist TEXT,
            rejected_by TEXT,
            rejected_reason TEXT,
            broker_order_id TEXT,
            broker_status TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_user_status ON pending_orders(user_id, status);
         CREATE INDEX IF NOT EXISTS idx_created_at ON pending_orders(created_at);",
    )?;
    if exists && !has_api_type {
        let owner: String = conn
            .query_row("SELECT username FROM users ORDER BY id LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_default();
        // The price is written as SQLite wrote it before 3.53, to 15
        // significant digits: 3.53 renders a REAL in JSON with up to 17, so
        // 0.1 + 0.2 would have become 0.30000000000000004 instead of 0.3.
        conn.execute(
            "INSERT INTO pending_orders (user_id, api_type, order_data, created_at, created_at_ist, status)
             SELECT ?1, 'placeorder',
                    json_object('symbol', symbol, 'exchange', exchange, 'action', side,
                                'quantity', quantity,
                                'price', CASE WHEN typeof(price) = 'real'
                                              THEN json(printf('%!.15g', price)) ELSE price END,
                                'pricetype', order_type, 'product', product, 'strategy', ''),
                    created_at, NULL,
                    CASE WHEN status IN ('pending', 'approved', 'rejected') THEN status ELSE 'rejected' END
             FROM pending_orders_legacy ORDER BY id",
            params![owner],
        )?;
        conn.execute_batch("DROP TABLE pending_orders_legacy")?;
    }
    Ok(())
}

/// Migration `089_pending_orders_force_live` (LOG-04): whether the caller
/// that queued the order had already decided it goes to the live broker
/// (`Route::force_live`, an OpenScript Live run), so its approval does not
/// re-read the analyzer toggle. A desktop column; the web has none. Checks
/// before altering.
pub fn migrate_force_live(conn: &Connection) -> Result<()> {
    if !super::migrations::column_exists(conn, "pending_orders", "force_live")? {
        conn.execute_batch(
            "ALTER TABLE pending_orders ADD COLUMN force_live INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

/// Migration `092_pending_orders_owner` (LOG-07): the broker and account an
/// order queued for a live destination was queued on, so its approval is
/// sent through no other session. Checks before altering.
pub fn migrate_owner(conn: &Connection) -> Result<()> {
    for column in ["broker", "broker_account"] {
        if !super::migrations::column_exists(conn, "pending_orders", column)? {
            conn.execute_batch(&format!(
                "ALTER TABLE pending_orders ADD COLUMN {} TEXT",
                column
            ))?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingOrderRow {
    pub id: i64,
    pub user_id: String,
    pub api_type: String,
    pub order_data: String,
    pub created_at: String,
    pub created_at_ist: Option<String>,
    pub status: String,
    pub approved_at: Option<String>,
    pub approved_at_ist: Option<String>,
    pub approved_by: Option<String>,
    pub rejected_at_ist: Option<String>,
    pub rejected_by: Option<String>,
    pub rejected_reason: Option<String>,
    pub broker_order_id: Option<String>,
    pub broker_status: Option<String>,
    /// Sent to the live broker on approval whatever analyzer mode says then.
    pub force_live: bool,
    /// The broker and account a live-destined order was queued on (LOG-07).
    pub broker: Option<String>,
    pub broker_account: Option<String>,
}

const COLS: &str = "id, user_id, api_type, order_data, created_at, created_at_ist, status, \
    approved_at, approved_at_ist, approved_by, rejected_at_ist, rejected_by, rejected_reason, \
    broker_order_id, broker_status, force_live, broker, broker_account";

fn map(r: &Row) -> rusqlite::Result<PendingOrderRow> {
    Ok(PendingOrderRow {
        id: r.get(0)?,
        user_id: r.get(1)?,
        api_type: r.get(2)?,
        order_data: r.get(3)?,
        created_at: r.get(4)?,
        created_at_ist: r.get(5)?,
        status: r.get(6)?,
        approved_at: r.get(7)?,
        approved_at_ist: r.get(8)?,
        approved_by: r.get(9)?,
        rejected_at_ist: r.get(10)?,
        rejected_by: r.get(11)?,
        rejected_reason: r.get(12)?,
        broker_order_id: r.get(13)?,
        broker_status: r.get(14)?,
        force_live: r.get(15)?,
        broker: r.get(16)?,
        broker_account: r.get(17)?,
    })
}

/// Naive UTC, as the web stores `DateTime` columns.
pub fn utc_text(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

/// Web `get_ist_timestamp`: `2026-10-05 10:00:00 IST`.
pub fn ist_text(now: DateTime<Utc>) -> String {
    now.with_timezone(&chrono_tz::Asia::Kolkata)
        .format("%Y-%m-%d %H:%M:%S IST")
        .to_string()
}

/// Where an approved order goes, as the caller that queued it decided.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Destination<'a> {
    /// The live broker whatever analyzer mode says at approval
    /// (`Route::force_live`).
    pub force_live: bool,
    /// The broker and account of the session a live order was queued on;
    /// its approval goes through no other (LOG-07).
    pub owner: Option<(&'a str, &'a str)>,
}

/// Queue one order for approval.
pub fn create(
    conn: &Connection,
    user_id: &str,
    api_type: &str,
    order_data: &str,
    dest: Destination<'_>,
    now: DateTime<Utc>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO pending_orders (user_id, api_type, order_data, created_at, created_at_ist,
                status, force_live, broker, broker_account)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8)",
        params![
            user_id,
            api_type,
            order_data,
            utc_text(now),
            ist_text(now),
            dest.force_live,
            dest.owner.map(|o| o.0),
            dest.owner.map(|o| o.1)
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// A user's orders, newest first, optionally of one status.
pub fn list(
    conn: &Connection,
    user_id: &str,
    status: Option<&str>,
) -> Result<Vec<PendingOrderRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM pending_orders WHERE user_id = ?1 AND (?2 IS NULL OR status = ?2)
         ORDER BY created_at DESC, id DESC"
    ))?;
    let rows = stmt.query_map(params![user_id, status], map)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<PendingOrderRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM pending_orders WHERE id = ?1"),
            params![id],
            map,
        )
        .optional()?)
}

/// pending -> approved, for one caller only.
pub fn approve(
    conn: &Connection,
    id: i64,
    by: &str,
    user_id: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE pending_orders SET status = 'approved', approved_by = ?1, approved_at = ?2,
                approved_at_ist = ?3
         WHERE id = ?4 AND user_id = ?5 AND status = 'pending'",
        params![by, utc_text(now), ist_text(now), id, user_id],
    )?;
    Ok(n == 1)
}

/// pending -> rejected, for one caller only.
pub fn reject(
    conn: &Connection,
    id: i64,
    reason: &str,
    by: &str,
    user_id: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE pending_orders SET status = 'rejected', rejected_reason = ?1, rejected_by = ?2,
                rejected_at = ?3, rejected_at_ist = ?4
         WHERE id = ?5 AND user_id = ?6 AND status = 'pending'",
        params![reason, by, utc_text(now), ist_text(now), id, user_id],
    )?;
    Ok(n == 1)
}

/// Claim an approved order for sending, at most once.
pub fn claim(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE pending_orders SET broker_status = ?1
         WHERE id = ?2 AND status = 'approved' AND broker_status IS NULL",
        params![SUBMITTING, id],
    )?;
    Ok(n == 1)
}

/// Put an approved order that was never sent back in the pending list.
pub fn return_to_pending(conn: &Connection, id: i64, broker_status: Option<&str>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE pending_orders SET status = 'pending', broker_status = NULL, broker_order_id = NULL,
                approved_by = NULL, approved_at = NULL, approved_at_ist = NULL
         WHERE id = ?1 AND status = 'approved' AND broker_status IS ?2",
        params![id, broker_status],
    )?;
    Ok(n == 1)
}

/// Delete an order that is no longer pending.
pub fn delete(conn: &Connection, id: i64, user_id: &str) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM pending_orders WHERE id = ?1 AND user_id = ?2 AND status != 'pending'",
        params![id, user_id],
    )?;
    Ok(n == 1)
}

pub fn update_broker_status(
    conn: &Connection,
    id: i64,
    broker_order_id: Option<&str>,
    broker_status: &str,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE pending_orders SET broker_order_id = ?1, broker_status = ?2 WHERE id = ?3",
        params![broker_order_id, broker_status, id],
    )?;
    Ok(n == 1)
}

pub fn pending_count(conn: &Connection, user_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM pending_orders WHERE user_id = ?1 AND status = 'pending'",
        params![user_id],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT);")
            .unwrap();
        migrate(&c).unwrap();
        migrate_force_live(&c).unwrap();
        migrate_owner(&c).unwrap();
        c
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 5, 4, 30, 0).unwrap()
    }

    #[test]
    fn transitions_are_compare_and_set() {
        let c = db();
        let id = create(&c, "u", "placeorder", "{}", Destination::default(), now()).unwrap();
        assert_eq!(pending_count(&c, "u").unwrap(), 1);
        assert!(
            !delete(&c, id, "u").unwrap(),
            "a pending order is not deleted"
        );
        assert!(!approve(&c, id, "x", "other", now()).unwrap(), "owner only");
        assert!(approve(&c, id, "u", "u", now()).unwrap());
        assert!(
            !approve(&c, id, "u", "u", now()).unwrap(),
            "second approve loses"
        );
        assert!(
            !reject(&c, id, "r", "u", "u", now()).unwrap(),
            "reject after approve loses"
        );
        assert!(claim(&c, id).unwrap());
        assert!(!claim(&c, id).unwrap(), "claimed once");
        assert!(return_to_pending(&c, id, Some(SUBMITTING)).unwrap());
        let row = get(&c, id).unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.broker_status), ("pending", None));
        assert!(reject(&c, id, "no", "u", "u", now()).unwrap());
        assert!(delete(&c, id, "u").unwrap());
        assert!(get(&c, id).unwrap().is_none());
    }

    #[test]
    fn legacy_placeholder_rows_are_carried_over() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT);
             INSERT INTO users (username) VALUES ('trader');
             CREATE TABLE pending_orders (id INTEGER PRIMARY KEY AUTOINCREMENT, strategy_id INTEGER,
               symbol TEXT NOT NULL, exchange TEXT NOT NULL, side TEXT NOT NULL, quantity INTEGER NOT NULL,
               price REAL NOT NULL DEFAULT 0, order_type TEXT NOT NULL DEFAULT 'MARKET',
               product TEXT NOT NULL DEFAULT 'MIS', status TEXT NOT NULL DEFAULT 'pending',
               created_at TEXT NOT NULL DEFAULT (datetime('now')), processed_at TEXT);
             INSERT INTO pending_orders (symbol, exchange, side, quantity) VALUES ('SBIN', 'NSE', 'BUY', 5);",
        )
        .unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        migrate_force_live(&c).unwrap();
        migrate_owner(&c).unwrap();
        let rows = list(&c, "trader", None).unwrap();
        assert_eq!(rows.len(), 1);
        let data: serde_json::Value = serde_json::from_str(&rows[0].order_data).unwrap();
        assert_eq!(data["symbol"], "SBIN");
        assert_eq!(data["action"], "BUY");
        assert_eq!(data["quantity"], 5);
        assert_eq!(rows[0].api_type, "placeorder");
    }

    /// A legacy row's price reaches the Action Center (and the order an
    /// approval places) as the text earlier builds wrote. Each expected
    /// price is what SQLite 3.50.4, which renders a REAL in JSON to 15
    /// digits, wrote for the same row; the bundled 3.54 renders up to 17
    /// (`0.30000000000000004` for 0.1 + 0.2).
    #[test]
    fn legacy_prices_keep_the_json_earlier_builds_wrote() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT);
             INSERT INTO users (username) VALUES ('trader');
             CREATE TABLE pending_orders (id INTEGER PRIMARY KEY AUTOINCREMENT, strategy_id INTEGER,
               symbol TEXT NOT NULL, exchange TEXT NOT NULL, side TEXT NOT NULL, quantity INTEGER NOT NULL,
               price REAL NOT NULL DEFAULT 0, order_type TEXT NOT NULL DEFAULT 'MARKET',
               product TEXT NOT NULL DEFAULT 'MIS', status TEXT NOT NULL DEFAULT 'pending',
               created_at TEXT NOT NULL DEFAULT (datetime('now')), processed_at TEXT);",
        )
        .unwrap();
        let cases: [(f64, &str); 11] = [
            (0.0, "0.0"),
            (100.0, "100.0"),
            (100.5, "100.5"),
            (2345.65, "2345.65"),
            (0.05, "0.05"),
            (1234.55, "1234.55"),
            (19999.95, "19999.95"),
            (0.1 + 0.2, "0.3"),
            (49.47, "49.47"),
            (123.456789012345, "123.456789012345"),
            (1e20, "1.0e+20"),
        ];
        for (price, _) in cases {
            c.execute(
                "INSERT INTO pending_orders (symbol, exchange, side, quantity, price, order_type)
                 VALUES ('SBIN', 'NSE', 'BUY', 5, ?1, 'LIMIT')",
                [price],
            )
            .unwrap();
        }
        migrate(&c).unwrap();
        let mut st = c
            .prepare("SELECT order_data FROM pending_orders ORDER BY id")
            .unwrap();
        let written: Vec<String> = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let expected: Vec<String> = cases
            .iter()
            .map(|(_, p)| {
                format!(
                    r#"{{"symbol":"SBIN","exchange":"NSE","action":"BUY","quantity":5,"price":{p},"pricetype":"LIMIT","product":"MIS","strategy":""}}"#
                )
            })
            .collect();
        assert_eq!(written, expected);
    }

    /// Every migration and when it was recorded, and every queued order's
    /// JSON, of the database at `path`.
    fn recorded_state(path: &std::path::Path) -> (Vec<(String, String)>, Vec<String>) {
        let c = Connection::open(path).unwrap();
        let mut st = c
            .prepare("SELECT name, applied_at FROM migrations ORDER BY name")
            .unwrap();
        let migrations = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let mut st = c
            .prepare("SELECT order_data FROM pending_orders ORDER BY id")
            .unwrap();
        let orders = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        (migrations, orders)
    }

    /// A database 1.0.6 migrated opens on this build. 1.0.7 adds no
    /// migration to `openalgo.db` and changes only the SQL of 060, and the
    /// runner matches a migration by its name, never by its SQL: the open
    /// succeeds, no migration is recorded again, and the order 060 carried
    /// over keeps the JSON 1.0.6 wrote (its price as SQLite 3.46 rendered
    /// 0.1 + 0.2). The database is built as 1.0.6 left it: the placeholder
    /// table of the legacy schema with a queued order, then the migrations.
    #[test]
    fn a_database_migrated_by_1_0_6_opens_without_running_060_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openalgo.db");
        {
            let c = Connection::open(&path).unwrap();
            crate::db::sqlite::migrations::run_legacy_schema(&c).unwrap();
            c.execute(
                "INSERT INTO pending_orders (symbol, exchange, side, quantity, price, order_type)
                 VALUES ('SBIN', 'NSE', 'BUY', 5, ?1, 'LIMIT')",
                [0.1 + 0.2],
            )
            .unwrap();
            crate::db::sqlite::migrations::run_migrations(&c).unwrap();
        }
        let before = recorded_state(&path);
        assert!(before
            .0
            .iter()
            .any(|(name, _)| name == "060_action_center_pending_orders"));
        assert_eq!(
            before.1,
            [
                r#"{"symbol":"SBIN","exchange":"NSE","action":"BUY","quantity":5,"price":0.3,"pricetype":"LIMIT","product":"MIS","strategy":""}"#
            ]
        );
        let db = crate::db::sqlite::SqliteDb::new(&path).unwrap();
        drop(db);
        assert_eq!(recorded_state(&path), before);
    }
}
