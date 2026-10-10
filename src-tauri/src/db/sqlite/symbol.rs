//! Symbol master persistence (`symtoken`, web `SymToken` columns).
//!
//! The table is a cache of the broker's master contract: it is replaced
//! wholesale on every download and read once at start-up into the
//! in-memory `SymbolResolver`. Runtime lookups never touch SQLite.

use crate::brokers::types::MasterContract;
use crate::error::Result;
use crate::state::SymbolInfo;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

/// Columns, in the web's order.
const COLUMNS: &str =
    "symbol, brsymbol, name, exchange, brexchange, token, expiry, strike, lotsize, instrumenttype, tick_size";

const CREATE_TABLE: &str = r#"
CREATE TABLE symtoken (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL,
    brsymbol TEXT NOT NULL,
    name TEXT,
    exchange TEXT,
    brexchange TEXT,
    token TEXT,
    expiry TEXT,
    strike REAL,
    lotsize INTEGER,
    instrumenttype TEXT,
    tick_size REAL
)"#;

const CREATE_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_symtoken_symbol_exchange ON symtoken(symbol, exchange);
CREATE INDEX IF NOT EXISTS idx_symtoken_brsymbol_exchange ON symtoken(brsymbol, exchange);
CREATE INDEX IF NOT EXISTS idx_symtoken_token ON symtoken(token);
CREATE INDEX IF NOT EXISTS idx_symtoken_exchange ON symtoken(exchange);
CREATE INDEX IF NOT EXISTS idx_symtoken_name_exchange ON symtoken(name, exchange);
"#;

fn column_exists(conn: &Connection, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(symtoken)")?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(names.iter().any(|n| n == column))
}

fn table_exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='symtoken')",
        [],
        |r| r.get(0),
    )?)
}

/// Migration 043: rebuild `symtoken` with every web `SymToken` column
/// (`brsymbol`, `brexchange`, `expiry`, `strike`, `lotsize`,
/// `instrumenttype`, `tick_size`) and no `UNIQUE(exchange, symbol)` (brokers
/// list duplicate symbols; rows are distinct by token). Idempotent: a table
/// that already has `lotsize` is left alone. Existing rows are carried over
/// (old `lot_size` / `instrument_type` / nullable broker columns backfilled
/// from what is there) so a populated master survives the upgrade.
pub fn migrate_symtoken(conn: &Connection) -> Result<()> {
    if table_exists(conn)? && column_exists(conn, "lotsize")? {
        conn.execute_batch(CREATE_INDEXES)?;
        return Ok(());
    }
    if !table_exists(conn)? {
        conn.execute_batch(CREATE_TABLE)?;
        conn.execute_batch(CREATE_INDEXES)?;
        return Ok(());
    }
    let has = |c: &str| column_exists(conn, c);
    let brsymbol = if has("brsymbol")? {
        "COALESCE(NULLIF(brsymbol, ''), symbol)"
    } else {
        "symbol"
    };
    let brexchange = if has("brexchange")? {
        "COALESCE(NULLIF(brexchange, ''), exchange)"
    } else {
        "exchange"
    };
    let expiry = if has("expiry")? {
        "COALESCE(expiry, '')"
    } else {
        "''"
    };
    let strike = if has("strike")? {
        "COALESCE(strike, 0)"
    } else {
        "0"
    };
    let lotsize = if has("lot_size")? { "lot_size" } else { "1" };
    let itype = if has("instrument_type")? {
        "instrument_type"
    } else {
        "'EQ'"
    };
    let tick = if has("tick_size")? {
        "tick_size"
    } else {
        "0.05"
    };
    conn.execute_batch("ALTER TABLE symtoken RENAME TO symtoken_old")?;
    // Old indexes keep their names after the rename; drop them so the new
    // table can use the canonical names.
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_symtoken_exchange;
         DROP INDEX IF EXISTS idx_symtoken_token;
         DROP INDEX IF EXISTS idx_symtoken_symbol;
         DROP INDEX IF EXISTS idx_symtoken_brsymbol;",
    )?;
    conn.execute_batch(CREATE_TABLE)?;
    conn.execute_batch(&format!(
        "INSERT INTO symtoken ({cols})
         SELECT symbol, {brsymbol}, name, exchange, {brexchange}, token, {expiry}, {strike},
                {lotsize}, {itype}, {tick}
         FROM symtoken_old ORDER BY id",
        cols = COLUMNS,
    ))?;
    conn.execute_batch("DROP TABLE symtoken_old")?;
    conn.execute_batch(CREATE_INDEXES)?;
    Ok(())
}

/// Which broker the stored master belongs to, written in the same
/// transaction as the rows so the two can never disagree. The download
/// history in `master_contract_status` is written afterwards and can lag
/// behind: a download aborted at logout still commits its rows on the
/// blocking thread.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterOwner {
    pub broker: String,
    pub rows: i64,
    pub stored_at: DateTime<Utc>,
}

/// Migration `078_symtoken_owner`: the one-row owner table. No backfill: a
/// master stored before it has no owner, which reads as unknown and makes
/// the next sign-in download once. The download history cannot stand in
/// for the owner, because it is the record that can disagree with the rows.
pub fn migrate_owner(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS symtoken_owner (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            broker TEXT NOT NULL,
            row_count INTEGER NOT NULL,
            stored_at TEXT NOT NULL
        );",
    )?;
    Ok(())
}

/// The stored master's owner; `None` when unknown (nothing stored since the
/// owner table was added).
pub fn owner(conn: &Connection) -> Result<Option<MasterOwner>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='symtoken_owner')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let row = conn
        .query_row(
            "SELECT broker, row_count, stored_at FROM symtoken_owner WHERE id = 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(|(broker, rows, at)| MasterOwner {
        broker,
        rows,
        stored_at: DateTime::parse_from_rfc3339(&at)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_default(),
    }))
}

/// Replace the whole master in one transaction, recording `broker` as its
/// owner in the same transaction. Indexes are dropped for the bulk insert
/// and rebuilt once at the end, which is several times faster than
/// maintaining them per row on a 100k-row master.
pub fn store_symbols(
    conn: &mut Connection,
    broker: &str,
    now: DateTime<Utc>,
    symbols: &[SymbolInfo],
) -> Result<()> {
    store_rows(conn, broker, now, symbols, &HashMap::new(), &HashMap::new())
}

/// Migration 065 (web `upgrade/migrate_contract_value.py`): the
/// `contract_value` column crypto masters carry (0.001 BTC per BTCUSD
/// contract). Idempotent: added only when missing. Existing rows keep NULL,
/// which every reader treats as "no multiplier" (1); the web's DEFAULT 1.0
/// is not written so no value is invented for rows that never had one.
pub fn migrate_contract_value(conn: &Connection) -> Result<()> {
    if !table_exists(conn)? || column_exists(conn, "contract_value")? {
        return Ok(());
    }
    conn.execute_batch("ALTER TABLE symtoken ADD COLUMN contract_value REAL")?;
    Ok(())
}

/// Replace the whole master with `broker`'s download, contract multipliers
/// (crypto) included, in one transaction with its owner like
/// `store_symbols`.
pub fn store_master(
    conn: &mut Connection,
    broker: &str,
    now: DateTime<Utc>,
    master: &MasterContract,
) -> Result<()> {
    store_rows(
        conn,
        broker,
        now,
        &master.rows,
        &master.contract_values,
        &master.lot_sizes,
    )
}

/// Contract multipliers by token (rows that have one).
pub fn load_contract_values(conn: &Connection) -> Result<HashMap<String, f64>> {
    if !column_exists(conn, "contract_value")? {
        return Ok(HashMap::new());
    }
    let mut stmt = conn.prepare(
        "SELECT token, contract_value FROM symtoken WHERE contract_value IS NOT NULL AND token IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<std::result::Result<HashMap<_, _>, _>>()?;
    Ok(rows)
}

/// Exact lot sizes by token of the rows whose `lotsize` is not a whole
/// number (crypto spot, stored as REAL like the web's float, MC-04).
pub fn load_lot_sizes(conn: &Connection) -> Result<HashMap<String, f64>> {
    let mut stmt = conn.prepare(
        "SELECT token, lotsize FROM symtoken WHERE typeof(lotsize) = 'real' AND token IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<std::result::Result<HashMap<_, _>, _>>()?;
    Ok(rows)
}

/// Load the whole master with its contract multipliers and exact lot
/// sizes (start-up).
pub fn load_master(conn: &Connection) -> Result<MasterContract> {
    Ok(MasterContract {
        rows: load_symbols(conn)?,
        contract_values: load_contract_values(conn)?,
        lot_sizes: load_lot_sizes(conn)?,
    })
}

fn store_rows(
    conn: &mut Connection,
    broker: &str,
    now: DateTime<Utc>,
    symbols: &[SymbolInfo],
    contract_values: &HashMap<String, f64>,
    lot_sizes: &HashMap<String, f64>,
) -> Result<()> {
    let start = std::time::Instant::now();
    let with_cv = column_exists(conn, "contract_value")?;
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM symtoken", [])?;
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_symtoken_symbol_exchange;
         DROP INDEX IF EXISTS idx_symtoken_brsymbol_exchange;
         DROP INDEX IF EXISTS idx_symtoken_token;
         DROP INDEX IF EXISTS idx_symtoken_exchange;
         DROP INDEX IF EXISTS idx_symtoken_name_exchange;",
    )?;
    {
        let sql = if with_cv {
            format!(
                "INSERT INTO symtoken ({}, contract_value) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                COLUMNS
            )
        } else {
            format!(
                "INSERT INTO symtoken ({}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                COLUMNS
            )
        };
        let mut stmt = tx.prepare(&sql)?;
        for s in symbols {
            // A fractional lot (crypto spot) is stored as the web stores
            // it: a REAL in the `lotsize` column.
            let lot = match lot_sizes.get(&s.token) {
                Some(v) => rusqlite::types::Value::Real(*v),
                None => rusqlite::types::Value::Integer(i64::from(s.lot_size)),
            };
            let base = params![
                s.symbol,
                s.brsymbol,
                s.name,
                s.exchange,
                s.brexchange,
                s.token,
                s.expiry,
                s.strike,
                lot,
                s.instrument_type,
                s.tick_size,
            ];
            if with_cv {
                let cv = contract_values.get(&s.token).copied();
                let mut all: Vec<&dyn rusqlite::ToSql> = base.to_vec();
                all.push(&cv);
                stmt.execute(all.as_slice())?;
            } else {
                stmt.execute(base)?;
            }
        }
    }
    tx.execute_batch(CREATE_INDEXES)?;
    migrate_owner(&tx)?;
    tx.execute(
        "INSERT INTO symtoken_owner (id, broker, row_count, stored_at) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET broker = ?1, row_count = ?2, stored_at = ?3",
        params![broker, symbols.len() as i64, now.to_rfc3339()],
    )?;
    tx.commit()?;
    tracing::info!(
        "Stored {} instruments in {:.2}s",
        symbols.len(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolInfo> {
    Ok(SymbolInfo {
        symbol: r.get(0)?,
        brsymbol: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        exchange: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        brexchange: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        token: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        expiry: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        strike: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
        // A fractional REAL (crypto spot, `load_lot_sizes`) is lot 1 here.
        lot_size: match r.get::<_, rusqlite::types::Value>(8)? {
            rusqlite::types::Value::Integer(i) => i32::try_from(i).unwrap_or(1),
            rusqlite::types::Value::Real(f)
                if f.fract() == 0.0 && f >= 1.0 && f <= f64::from(i32::MAX) =>
            {
                f as i32
            }
            _ => 1,
        },
        instrument_type: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
        tick_size: r.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
    })
}

/// Load the whole master (start-up). The caller hands it straight to the
/// resolver, which keeps the only copy.
pub fn load_symbols(conn: &Connection) -> Result<Vec<SymbolInfo>> {
    let mut stmt = conn.prepare(&format!("SELECT {} FROM symtoken ORDER BY id", COLUMNS))?;
    let rows = stmt
        .query_map([], row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The stored rows of one instrument type, in master order: what a broker
/// carries into a download when part of it fails (Firstock's index rows,
/// web `get_existing_index_rows`). Empty when there is no table.
pub fn load_by_instrument_type(conn: &Connection, itype: &str) -> Result<Vec<SymbolInfo>> {
    if !table_exists(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM symtoken WHERE instrumenttype = ?1 ORDER BY id",
        COLUMNS
    ))?;
    let rows = stmt
        .query_map([itype], row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn count_symbols(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM symtoken", [], |r| r.get(0))?)
}

/// Instruments per exchange (web `get_exchange_stats_from_db`).
pub fn exchange_counts(conn: &Connection) -> Result<std::collections::BTreeMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT exchange, COUNT(*) FROM symtoken WHERE exchange IS NOT NULL GROUP BY exchange",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::common::symbols::tests::row as sym;

    fn legacy_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_legacy_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn migration_rebuilds_a_populated_legacy_table() {
        let conn = legacy_db();
        conn.execute_batch(
            "INSERT INTO symtoken (symbol, token, exchange, name, lot_size, tick_size, instrument_type, expiry, strike, brsymbol, brexchange)
             VALUES ('SBIN', '3045', 'NSE', 'SBIN', 1, 0.05, 'EQ', NULL, NULL, 'SBIN-EQ', 'NSE'),
                    ('NIFTY27OCT26FUT', '9', 'NFO', 'NIFTY', 65, 0.1, 'FUT', '27-OCT-26', 0, NULL, NULL);",
        )
        .unwrap();
        migrate_symtoken(&conn).unwrap();
        let rows = load_symbols(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].brsymbol, "SBIN-EQ");
        assert_eq!(rows[0].expiry, "");
        assert_eq!(rows[1].brsymbol, "NIFTY27OCT26FUT");
        assert_eq!(rows[1].brexchange, "NFO");
        assert_eq!(rows[1].lot_size, 65);
        assert_eq!(rows[1].expiry, "27-OCT-26");
        assert_eq!(rows[1].instrument_type, "FUT");
        // Idempotent.
        migrate_symtoken(&conn).unwrap();
        assert_eq!(count_symbols(&conn).unwrap(), 2);
        assert!(column_exists(&conn, "instrumenttype").unwrap());
        assert!(!column_exists(&conn, "lot_size").unwrap());
    }

    #[test]
    fn full_run_creates_the_web_schema_and_round_trips_every_column() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        let mut conn = conn;
        let mut fut = sym(
            "NIFTY27OCT26FUT",
            "NIFTY26OCTFUT",
            "NFO",
            "10011906::::39109",
        );
        fut.expiry = "27-OCT-26".into();
        fut.strike = 0.0;
        fut.lot_size = 65;
        fut.instrument_type = "FUT".into();
        fut.tick_size = 0.1;
        let mut opt = fut.clone();
        opt.symbol = "NIFTY27OCT2625000CE".into();
        opt.token = "2".into();
        opt.strike = 25000.0;
        opt.instrument_type = "CE".into();
        // Duplicate symbol on one exchange is allowed (no UNIQUE constraint).
        let mut dup = opt.clone();
        dup.token = "3".into();
        store_symbols(
            &mut conn,
            "zerodha",
            Utc::now(),
            &[fut.clone(), opt.clone(), dup],
        )
        .unwrap();
        let rows = load_symbols(&conn).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], fut);
        assert_eq!(rows[1], opt);
        // Replaced wholesale.
        store_symbols(&mut conn, "zerodha", Utc::now(), &[fut.clone()]).unwrap();
        assert_eq!(count_symbols(&conn).unwrap(), 1);
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND tbl_name='symtoken'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(idx >= 5);
    }

    #[test]
    fn bulk_store_is_fast() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        let mut conn = conn;
        let rows: Vec<SymbolInfo> = (0..100_000)
            .map(|i| {
                sym(
                    &format!("S{}", i),
                    &format!("S{}-EQ", i),
                    "NSE",
                    &i.to_string(),
                )
            })
            .collect();
        let t = std::time::Instant::now();
        store_symbols(&mut conn, "zerodha", Utc::now(), &rows).unwrap();
        let loaded = load_symbols(&conn).unwrap();
        assert_eq!(loaded.len(), 100_000);
        // Generous bound for slow CI runners; locally this is well under 2 s.
        assert!(t.elapsed() < std::time::Duration::from_secs(30));
    }

    #[test]
    fn contract_value_migration_keeps_a_populated_master() {
        // A master stored before 065 (column absent), then the migration.
        let conn = legacy_db();
        migrate_symtoken(&conn).unwrap();
        let mut conn = conn;
        store_symbols(
            &mut conn,
            "zerodha",
            Utc::now(),
            &[sym("SBIN", "SBIN-EQ", "NSE", "3045")],
        )
        .unwrap();
        assert!(!column_exists(&conn, "contract_value").unwrap());
        assert!(load_contract_values(&conn).unwrap().is_empty());
        migrate_contract_value(&conn).unwrap();
        assert!(column_exists(&conn, "contract_value").unwrap());
        // Existing rows survive with no invented multiplier.
        assert_eq!(load_symbols(&conn).unwrap().len(), 1);
        assert!(load_contract_values(&conn).unwrap().is_empty());
        // Idempotent.
        migrate_contract_value(&conn).unwrap();
        // A crypto master round-trips its multipliers.
        let mut cv = HashMap::new();
        cv.insert("27".to_string(), 0.001);
        let master = MasterContract {
            rows: vec![
                sym("BTCUSDFUT", "BTCUSD", "CRYPTO", "27"),
                sym("BTCINR", "BTC_INR", "CRYPTO", "1600"),
            ],
            contract_values: cv,
            ..Default::default()
        };
        store_master(&mut conn, "deltaexchange", Utc::now(), &master).unwrap();
        let back = load_master(&conn).unwrap();
        assert_eq!(back.rows, master.rows);
        assert_eq!(back.contract_values, master.contract_values);
        // A plain store (Indian broker) clears them with the rows.
        store_symbols(
            &mut conn,
            "zerodha",
            Utc::now(),
            &[sym("SBIN", "SBIN-EQ", "NSE", "3045")],
        )
        .unwrap();
        assert!(load_contract_values(&conn).unwrap().is_empty());
    }

    #[test]
    fn full_run_adds_the_contract_value_column() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        assert!(column_exists(&conn, "contract_value").unwrap());
        // Running the whole chain again changes nothing.
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        assert!(column_exists(&conn, "contract_value").unwrap());
    }

    /// MC-04: a fractional lot (Delta spot, 0.0001 BTC) is stored as a REAL
    /// like the web's float and comes back exact; whole lots stay integers.
    #[test]
    fn fractional_lots_round_trip() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        let mut fut = sym("BTCUSDFUT", "BTCUSD", "CRYPTO", "27");
        fut.lot_size = 1;
        let spot = sym("BTCINR", "BTC_INR", "CRYPTO", "1600");
        let mut nifty = sym("NIFTY27OCT26FUT", "NIFTY26OCTFUT", "NFO", "9");
        nifty.lot_size = 65;
        let mut lots = HashMap::new();
        lots.insert("1600".to_string(), 0.0001);
        let master = MasterContract {
            rows: vec![fut, spot, nifty],
            lot_sizes: lots.clone(),
            ..Default::default()
        };
        store_master(&mut conn, "deltaexchange", Utc::now(), &master).unwrap();
        let kinds: Vec<String> = conn
            .prepare("SELECT typeof(lotsize) FROM symtoken ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(kinds, ["integer", "real", "integer"]);
        let back = load_master(&conn).unwrap();
        assert_eq!(back.lot_sizes, lots);
        assert_eq!(back.rows, master.rows);
        assert_eq!(back.rows[1].lot_size, 1);
        assert_eq!(back.rows[2].lot_size, 65);
    }

    /// MC-01: the owner is written with the rows, in one transaction; a
    /// store that fails keeps both the previous rows and their owner.
    #[test]
    fn the_owner_travels_with_the_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        assert_eq!(owner(&conn).unwrap(), None);
        let t0 = Utc::now();
        let sbin = sym("SBIN", "SBIN-EQ", "NSE", "3045");
        store_symbols(&mut conn, "zerodha", t0, &[sbin.clone()]).unwrap();
        let o = owner(&conn).unwrap().unwrap();
        assert_eq!((o.broker.as_str(), o.rows), ("zerodha", 1));
        assert_eq!(o.stored_at.timestamp(), t0.timestamp());
        store_master(
            &mut conn,
            "angel",
            t0,
            &MasterContract::new(vec![sbin.clone(), sym("INFY", "INFY-EQ", "NSE", "1594")]),
        )
        .unwrap();
        assert_eq!(owner(&conn).unwrap().unwrap().broker, "angel");
        assert_eq!(owner(&conn).unwrap().unwrap().rows, 2);
        // A failing insert rolls back the rows and the owner together.
        conn.execute_batch(
            "CREATE TRIGGER refuse BEFORE INSERT ON symtoken WHEN NEW.symbol = 'BAD'
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
        let bad = sym("BAD", "BAD-EQ", "NSE", "1");
        assert!(store_symbols(&mut conn, "upstox", t0, &[sbin, bad]).is_err());
        assert_eq!(owner(&conn).unwrap().unwrap().broker, "angel");
        assert_eq!(count_symbols(&conn).unwrap(), 2);
    }

    /// Migration 078 on a populated master: no owner is invented, so the
    /// next sign-in downloads once.
    #[test]
    fn owner_migration_keeps_a_populated_master_with_no_owner() {
        let conn = legacy_db();
        migrate_symtoken(&conn).unwrap();
        let mut conn = conn;
        // Rows stored before 078: insert them without the owner table.
        conn.execute(
            "INSERT INTO symtoken (symbol, brsymbol, exchange, token) VALUES ('SBIN', 'SBIN-EQ', 'NSE', '3045')",
            [],
        )
        .unwrap();
        assert_eq!(owner(&conn).unwrap(), None);
        migrate_owner(&conn).unwrap();
        migrate_owner(&conn).unwrap();
        assert_eq!(owner(&conn).unwrap(), None);
        assert_eq!(count_symbols(&conn).unwrap(), 1);
        store_symbols(
            &mut conn,
            "zerodha",
            Utc::now(),
            &[sym("SBIN", "SBIN-EQ", "NSE", "3045")],
        )
        .unwrap();
        assert_eq!(owner(&conn).unwrap().unwrap().broker, "zerodha");
    }
}
