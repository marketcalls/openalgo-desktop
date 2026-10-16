//! Replacing the symbol master without holding the database (DB-01).
//!
//! A master of 200,000 instruments replaced in one transaction held
//! `openalgo.db`'s write lock for 1.7 to 2.7 s on an M1, so every order
//! journal write (strategy and OpenScript order rows, scalping protection,
//! the Action Center queue) waited behind it at broker sign-in and at each
//! daily refresh. The master is replaced in stages instead:
//!
//! 1. `symtoken_next` is filled in short transactions ([`CHUNK_BUDGET`]),
//!    with a pause after each so a write waiting for the lock gets it;
//! 2. one short transaction swaps it in, renaming both tables, and records
//!    its owner (MC-01: the rows and their owner commit together);
//! 3. the previous table is dropped (pages go to the free list: about
//!    20 ms for 200,000 rows).
//!
//! Nothing reads the staging table: lookups use the in-memory resolver,
//! and the few SQL readers see the complete previous master until the swap.
//! A table an interrupted replacement left behind is dropped first thing
//! next time ([`clear_leftovers`]), so `symtoken` is always a complete
//! master.
//!
//! Index names follow a table through a rename and are unique, so the
//! staging table takes whichever of the two name sets ([`INDEX_SETS`]) the
//! live table is not using. A migration that changes `symtoken`'s indexes
//! goes through [`create_indexes`].

use super::symbol::{column_exists, migrate_owner, COLUMNS, CREATE_TABLE};
use crate::error::Result;
use crate::state::SymbolInfo;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, TransactionBehavior};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const LIVE: &str = "symtoken";
const STAGING: &str = "symtoken_next";
const PREVIOUS: &str = "symtoken_old";

/// The longest a staging transaction keeps adding rows before it commits.
const CHUNK_BUDGET: Duration = Duration::from_millis(25);
/// The most rows one staging transaction adds.
const CHUNK_ROWS: usize = 2_000;
/// The pause after each short transaction. SQLite's busy wait has no
/// queue: a writer that begins again at once starves a waiting one (a
/// back-to-back loop kept a small write waiting over 3 s). A waiting
/// connection retries every millisecond (`retry_busy`), or every timer tick
/// where sleeps are coarser (about 15.6 ms on Windows), so a pause longer
/// than a tick always lets it in.
const PAUSE: Duration = Duration::from_millis(20);

/// The indexes every master table carries: (name key, columns).
const INDEXES: [(&str, &str); 5] = [
    ("symbol_exchange", "symbol, exchange"),
    ("brsymbol_exchange", "brsymbol, exchange"),
    ("token", "token"),
    ("exchange", "exchange"),
    ("name_exchange", "name, exchange"),
];

/// The two index name sets, `idx_symtoken_<key>` (migration 043's) and
/// `idx_symtoken_<key>_b`; the live and the staging table never share one.
const INDEX_SETS: [&str; 2] = ["", "_b"];

/// The master's rows, with the crypto contract multipliers and the exact
/// fractional lot sizes, by token.
pub(super) struct MasterRows<'a> {
    pub rows: &'a [SymbolInfo],
    pub contract_values: &'a HashMap<String, f64>,
    pub lot_sizes: &'a HashMap<String, f64>,
}

/// Replace the master with `master`, owned by `broker`. Blocking: it
/// pauses between transactions; run it on a blocking thread.
pub(super) fn replace(
    conn: &mut Connection,
    broker: &str,
    now: DateTime<Utc>,
    master: &MasterRows<'_>,
) -> Result<()> {
    let started = Instant::now();
    clear_leftovers(conn)?;
    let with_cv = column_exists(conn, "contract_value")?;
    create_staging(conn, with_cv)?;
    if let Err(e) = fill(conn, master, with_cv) {
        // The live master is untouched; the half-filled table goes now, or
        // at the next replacement if this fails too.
        let _ = drop_table(conn, STAGING);
        return Err(e);
    }
    swap(conn, broker, now, master.rows.len())?;
    std::thread::sleep(PAUSE);
    drop_table(conn, PREVIOUS)?;
    tracing::info!(
        "Stored {} instruments in {:.2}s",
        master.rows.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Create the master indexes on `table` with the name set `set`.
pub(super) fn create_indexes(conn: &Connection, table: &str, set: &str) -> Result<()> {
    for (key, cols) in INDEXES {
        conn.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS idx_symtoken_{key}{set} ON {table}({cols})"
        ))?;
    }
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )?)
}

fn drop_table(conn: &mut Connection, table: &str) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(&format!("DROP TABLE IF EXISTS {}", table))?;
    tx.commit()?;
    Ok(())
}

/// Drop what an interrupted replacement left: a staging table (the live
/// master is the previous one, complete) or the previous table (the swap
/// had committed).
fn clear_leftovers(conn: &mut Connection) -> Result<()> {
    for table in [STAGING, PREVIOUS] {
        if table_exists(conn, table)? {
            tracing::info!(
                "Removing a master table an interrupted download left ({})",
                table
            );
            drop_table(conn, table)?;
            std::thread::sleep(PAUSE);
        }
    }
    Ok(())
}

/// The index name set no index in the database uses. Both in use (a
/// live table carrying both) frees the second by dropping its indexes.
fn free_index_set(conn: &Connection) -> Result<&'static str> {
    let mut st = conn.prepare("SELECT name FROM sqlite_master WHERE type='index'")?;
    let names = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    let used = |set: &str| {
        INDEXES
            .iter()
            .any(|(key, _)| names.contains(&format!("idx_symtoken_{key}{set}")))
    };
    if let Some(set) = INDEX_SETS.into_iter().find(|s| !used(s)) {
        return Ok(set);
    }
    let set = INDEX_SETS[1];
    for (key, _) in INDEXES {
        conn.execute_batch(&format!("DROP INDEX IF EXISTS idx_symtoken_{key}{set}"))?;
    }
    Ok(set)
}

/// The empty staging table, with the live table's columns and indexes.
fn create_staging(conn: &mut Connection, with_cv: bool) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(&CREATE_TABLE.replacen(LIVE, STAGING, 1))?;
    if with_cv {
        tx.execute_batch(&format!(
            "ALTER TABLE {} ADD COLUMN contract_value REAL",
            STAGING
        ))?;
    }
    let set = free_index_set(&tx)?;
    create_indexes(&tx, STAGING, set)?;
    tx.commit()?;
    Ok(())
}

/// Fill the staging table, one short transaction at a time.
fn fill(conn: &mut Connection, master: &MasterRows<'_>, with_cv: bool) -> Result<()> {
    let sql = if with_cv {
        format!(
            "INSERT INTO {} ({}, contract_value) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            STAGING, COLUMNS
        )
    } else {
        format!(
            "INSERT INTO {} ({}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            STAGING, COLUMNS
        )
    };
    let mut rest = master.rows;
    while !rest.is_empty() {
        let began = Instant::now();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut taken = 0;
        {
            let mut stmt = tx.prepare_cached(&sql)?;
            for s in rest.iter().take(CHUNK_ROWS) {
                insert_row(&mut stmt, s, master, with_cv)?;
                taken += 1;
                if taken % 100 == 0 && began.elapsed() >= CHUNK_BUDGET {
                    break;
                }
            }
        }
        tx.commit()?;
        rest = &rest[taken..];
        std::thread::sleep(PAUSE);
    }
    Ok(())
}

fn insert_row(
    stmt: &mut rusqlite::CachedStatement<'_>,
    s: &SymbolInfo,
    master: &MasterRows<'_>,
    with_cv: bool,
) -> Result<()> {
    // A fractional lot (crypto spot) is stored as the web stores it: a
    // REAL in the `lotsize` column.
    let lot = match master.lot_sizes.get(&s.token) {
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
        let cv = master.contract_values.get(&s.token).copied();
        let mut all: Vec<&dyn rusqlite::ToSql> = base.to_vec();
        all.push(&cv);
        stmt.execute(all.as_slice())?;
    } else {
        stmt.execute(base)?;
    }
    Ok(())
}

/// Swap the staging table in and record its owner, in one transaction.
fn swap(conn: &mut Connection, broker: &str, now: DateTime<Utc>, rows: usize) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(&format!(
        "ALTER TABLE {LIVE} RENAME TO {PREVIOUS}; ALTER TABLE {STAGING} RENAME TO {LIVE};"
    ))?;
    migrate_owner(&tx)?;
    tx.execute(
        "INSERT INTO symtoken_owner (id, broker, row_count, stored_at) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET broker = ?1, row_count = ?2, stored_at = ?3",
        params![broker, rows as i64, now.to_rfc3339()],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::migrations::run_migrations;
    use crate::db::sqlite::symbol::{count_symbols, load_symbols, owner, store_symbols};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;

    fn sym(symbol: &str, token: &str) -> SymbolInfo {
        SymbolInfo {
            symbol: symbol.into(),
            brsymbol: format!("{}-EQ", symbol),
            name: symbol.into(),
            exchange: "NSE".into(),
            brexchange: "NSE".into(),
            token: token.into(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "EQ".into(),
            tick_size: 0.05,
        }
    }

    fn master(tag: &str, n: usize) -> Vec<SymbolInfo> {
        (0..n)
            .map(|k| sym(&format!("{}{}", tag, k), &format!("{}-{}", tag, k)))
            .collect()
    }

    fn migrated() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn
    }

    fn live_indexes(conn: &Connection) -> Vec<String> {
        let mut st = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='symtoken' ORDER BY name")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    fn symbols(conn: &Connection) -> Vec<String> {
        load_symbols(conn)
            .unwrap()
            .into_iter()
            .map(|s| s.symbol)
            .collect()
    }

    /// Each replacement leaves the new master, its owner, the five indexes
    /// (the two name sets taking turns) and no staging or previous table.
    #[test]
    fn replacements_swap_in_complete_masters_and_leave_nothing_behind() {
        let mut conn = migrated();
        let now = Utc::now();
        for (i, (broker, set)) in [("zerodha", "_b"), ("angel", ""), ("dhan", "_b")]
            .into_iter()
            .enumerate()
        {
            let rows = master(&format!("M{}", i), 5);
            store_symbols(&mut conn, broker, now, &rows).unwrap();
            let want: Vec<String> = rows.iter().map(|s| s.symbol.clone()).collect();
            assert_eq!(symbols(&conn), want);
            let o = owner(&conn).unwrap().unwrap();
            assert_eq!((o.broker.as_str(), o.rows), (broker, 5));
            assert!(!table_exists(&conn, STAGING).unwrap());
            assert!(!table_exists(&conn, PREVIOUS).unwrap());
            let mut names: Vec<String> = INDEXES
                .iter()
                .map(|(key, _)| format!("idx_symtoken_{}{}", key, set))
                .collect();
            names.sort();
            assert_eq!(live_indexes(&conn), names, "replacement {}", i + 1);
        }
    }

    /// A staging that fails (or a crash while it runs) leaves the master
    /// and its owner as they were; the next replacement clears it.
    #[test]
    fn a_failed_staging_keeps_the_master_and_its_owner() {
        let mut conn = migrated();
        let now = Utc::now();
        store_symbols(&mut conn, "zerodha", now, &master("A", 2)).unwrap();
        create_staging(&mut conn, true).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse BEFORE INSERT ON symtoken_next WHEN NEW.symbol = 'BAD'
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
        let mut rows = master("B", 3);
        rows.push(sym("BAD", "bad"));
        let none = HashMap::new();
        let staged = MasterRows {
            rows: &rows,
            contract_values: &none,
            lot_sizes: &none,
        };
        assert!(fill(&mut conn, &staged, true).is_err());
        // Interrupted here: the staging table is left behind.
        assert!(table_exists(&conn, STAGING).unwrap());
        assert_eq!(symbols(&conn), ["A0", "A1"]);
        assert_eq!(owner(&conn).unwrap().unwrap().broker, "zerodha");
        store_symbols(&mut conn, "angel", now, &master("C", 1)).unwrap();
        assert_eq!(symbols(&conn), ["C0"]);
        assert_eq!(owner(&conn).unwrap().unwrap().broker, "angel");
        assert!(!table_exists(&conn, STAGING).unwrap());
    }

    /// A crash between the swap and the drop leaves the new master in
    /// place; the previous table goes at the next replacement.
    #[test]
    fn a_crash_after_the_swap_keeps_the_new_master() {
        let mut conn = migrated();
        let now = Utc::now();
        store_symbols(&mut conn, "zerodha", now, &master("A", 2)).unwrap();
        let rows = master("B", 3);
        let none = HashMap::new();
        let staged = MasterRows {
            rows: &rows,
            contract_values: &none,
            lot_sizes: &none,
        };
        create_staging(&mut conn, true).unwrap();
        fill(&mut conn, &staged, true).unwrap();
        swap(&mut conn, "angel", now, rows.len()).unwrap();
        assert!(table_exists(&conn, PREVIOUS).unwrap());
        assert_eq!(symbols(&conn), ["B0", "B1", "B2"]);
        assert_eq!(owner(&conn).unwrap().unwrap().broker, "angel");
        store_symbols(&mut conn, "dhan", now, &master("C", 1)).unwrap();
        assert!(!table_exists(&conn, PREVIOUS).unwrap());
        assert_eq!(count_symbols(&conn).unwrap(), 1);
    }

    /// DB-01: a small write (an order row) made while a large master is
    /// replaced waits for one short transaction, not the whole replacement.
    /// In one transaction it waited until the master was stored (1.7 to
    /// 2.7 s for 200,000 rows on an M1).
    #[test]
    fn a_write_during_a_master_replacement_does_not_wait_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::sqlite::open_pool(
            &dir.path().join("openalgo.db"),
            4,
            crate::db::sqlite::Durability::MAIN,
        )
        .unwrap();
        let mut conn = pool.get().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute_batch("CREATE TABLE order_rows (id INTEGER PRIMARY KEY, at TEXT)")
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worst_us = Arc::new(AtomicU64::new(0));
        let writes = Arc::new(AtomicU64::new(0));
        let writer = {
            let (pool, stop, worst_us, writes) =
                (pool.clone(), stop.clone(), worst_us.clone(), writes.clone());
            std::thread::spawn(move || {
                let w = pool.get().unwrap();
                while !stop.load(Ordering::SeqCst) {
                    let t = Instant::now();
                    w.execute("INSERT INTO order_rows (at) VALUES ('now')", [])
                        .unwrap();
                    worst_us.fetch_max(t.elapsed().as_micros() as u64, Ordering::SeqCst);
                    writes.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };
        let started = Instant::now();
        store_symbols(&mut conn, "zerodha", Utc::now(), &master("NIFTY", 60_000)).unwrap();
        let took = started.elapsed();
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap();
        assert_eq!(count_symbols(&conn).unwrap(), 60_000);
        let worst = Duration::from_micros(worst_us.load(Ordering::SeqCst));
        // A small part of the replacement, never the whole of it (one
        // transaction made the write wait for nearly all of it), and well
        // inside the 5 s busy budget. The time includes the write's own
        // commit and each short transaction's (fsyncs, at FULL): slow on a
        // CI runner's disk (946 ms of a 21 s debug replacement on Windows),
        // so the bound is relative, not a fixed few milliseconds.
        let bound = (took / 4).min(Duration::from_secs(2));
        assert!(
            worst < bound,
            "a write waited {:?} during a {:?} replacement",
            worst,
            took
        );
        assert!(
            writes.load(Ordering::SeqCst) >= 20,
            "the writer ran during the replacement"
        );
    }
}
