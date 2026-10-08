//! Historify schema: the web's `database/historify_db.py` tables and
//! columns, reached through numbered, idempotent steps.
//!
//! - `006_desktop_v1_to_web`: earlier desktop builds created their own
//!   tables (`timeframe` instead of `interval`, `TIMESTAMP` candles, integer
//!   job ids, list-based watchlist). Each such table is rebuilt in the web's
//!   shape and its rows carried over; nothing is dropped before its rows are
//!   copied. Checks the shape first, so a web-created file is untouched.
//! - Every open: create any missing web table, add any missing column (a
//!   file from an older web release), create the indexes and bring the ID
//!   sequences level with their tables (web `sync_id_sequences`).

use crate::error::{AppError, Result};
use duckdb::Connection;

/// The web's tables, verbatim (`init_database`).
const WEB_TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS market_data (
    symbol VARCHAR NOT NULL,
    exchange VARCHAR NOT NULL,
    interval VARCHAR NOT NULL,
    timestamp BIGINT NOT NULL,
    open DOUBLE NOT NULL,
    high DOUBLE NOT NULL,
    low DOUBLE NOT NULL,
    close DOUBLE NOT NULL,
    volume BIGINT NOT NULL,
    oi BIGINT DEFAULT 0,
    created_at TIMESTAMP DEFAULT current_timestamp,
    PRIMARY KEY (symbol, exchange, interval, timestamp)
);
CREATE TABLE IF NOT EXISTS watchlist (
    id INTEGER PRIMARY KEY,
    symbol VARCHAR NOT NULL,
    exchange VARCHAR NOT NULL,
    display_name VARCHAR,
    added_at TIMESTAMP DEFAULT current_timestamp,
    UNIQUE (symbol, exchange)
);
CREATE TABLE IF NOT EXISTS data_catalog (
    id INTEGER PRIMARY KEY,
    symbol VARCHAR NOT NULL,
    exchange VARCHAR NOT NULL,
    interval VARCHAR NOT NULL,
    first_timestamp BIGINT,
    last_timestamp BIGINT,
    record_count BIGINT DEFAULT 0,
    last_download_at TIMESTAMP,
    UNIQUE (symbol, exchange, interval)
);
CREATE TABLE IF NOT EXISTS download_jobs (
    id VARCHAR PRIMARY KEY,
    job_type VARCHAR NOT NULL,
    status VARCHAR NOT NULL,
    total_symbols INTEGER DEFAULT 0,
    completed_symbols INTEGER DEFAULT 0,
    failed_symbols INTEGER DEFAULT 0,
    interval VARCHAR,
    start_date VARCHAR,
    end_date VARCHAR,
    config VARCHAR,
    created_at TIMESTAMP DEFAULT current_timestamp,
    started_at TIMESTAMP,
    completed_at TIMESTAMP,
    error_message VARCHAR
);
CREATE TABLE IF NOT EXISTS job_items (
    id INTEGER PRIMARY KEY,
    job_id VARCHAR NOT NULL,
    symbol VARCHAR NOT NULL,
    exchange VARCHAR NOT NULL,
    status VARCHAR NOT NULL,
    records_downloaded INTEGER DEFAULT 0,
    error_message VARCHAR,
    started_at TIMESTAMP,
    completed_at TIMESTAMP
);
CREATE TABLE IF NOT EXISTS symbol_metadata (
    symbol VARCHAR NOT NULL,
    exchange VARCHAR NOT NULL,
    name VARCHAR,
    expiry VARCHAR,
    strike DOUBLE,
    lotsize INTEGER,
    instrumenttype VARCHAR,
    tick_size DOUBLE,
    last_updated TIMESTAMP DEFAULT current_timestamp,
    PRIMARY KEY (symbol, exchange)
);
CREATE TABLE IF NOT EXISTS historify_schedules (
    id VARCHAR PRIMARY KEY,
    name VARCHAR NOT NULL,
    description VARCHAR,
    schedule_type VARCHAR NOT NULL,
    interval_value INTEGER,
    interval_unit VARCHAR,
    time_of_day VARCHAR,
    download_source VARCHAR DEFAULT 'watchlist',
    data_interval VARCHAR NOT NULL,
    lookback_days INTEGER DEFAULT 1,
    is_enabled BOOLEAN DEFAULT TRUE,
    is_paused BOOLEAN DEFAULT FALSE,
    status VARCHAR DEFAULT 'idle',
    apscheduler_job_id VARCHAR,
    created_at TIMESTAMP DEFAULT current_timestamp,
    last_run_at TIMESTAMP,
    next_run_at TIMESTAMP,
    last_run_status VARCHAR,
    total_runs INTEGER DEFAULT 0,
    successful_runs INTEGER DEFAULT 0,
    failed_runs INTEGER DEFAULT 0
);
CREATE TABLE IF NOT EXISTS historify_schedule_executions (
    id INTEGER PRIMARY KEY,
    schedule_id VARCHAR NOT NULL,
    download_job_id VARCHAR,
    status VARCHAR NOT NULL,
    started_at TIMESTAMP DEFAULT current_timestamp,
    completed_at TIMESTAMP,
    symbols_processed INTEGER DEFAULT 0,
    symbols_success INTEGER DEFAULT 0,
    symbols_failed INTEGER DEFAULT 0,
    records_downloaded INTEGER DEFAULT 0,
    error_message VARCHAR
);
"#;

/// No secondary index on `market_data` (web #1779: ART memory stays resident
/// and OOMs large 1m backfills; zone maps serve the range scans).
const WEB_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_job_items_job_id ON job_items (job_id);
CREATE INDEX IF NOT EXISTS idx_download_jobs_status ON download_jobs (status);
CREATE INDEX IF NOT EXISTS idx_historify_schedules_enabled ON historify_schedules (is_enabled, is_paused);
CREATE INDEX IF NOT EXISTS idx_historify_schedule_executions_schedule_id ON historify_schedule_executions (schedule_id);
"#;

/// Columns a file from an older web release may lack (table, column, type).
const WEB_COLUMNS: &[(&str, &str, &str)] = &[
    ("market_data", "oi", "BIGINT DEFAULT 0"),
    ("market_data", "created_at", "TIMESTAMP"),
    ("watchlist", "display_name", "VARCHAR"),
    ("watchlist", "added_at", "TIMESTAMP"),
    ("data_catalog", "first_timestamp", "BIGINT"),
    ("data_catalog", "last_timestamp", "BIGINT"),
    ("data_catalog", "record_count", "BIGINT DEFAULT 0"),
    ("data_catalog", "last_download_at", "TIMESTAMP"),
    ("download_jobs", "failed_symbols", "INTEGER DEFAULT 0"),
    ("download_jobs", "interval", "VARCHAR"),
    ("download_jobs", "start_date", "VARCHAR"),
    ("download_jobs", "end_date", "VARCHAR"),
    ("download_jobs", "config", "VARCHAR"),
    ("download_jobs", "started_at", "TIMESTAMP"),
    ("download_jobs", "completed_at", "TIMESTAMP"),
    ("download_jobs", "error_message", "VARCHAR"),
    ("job_items", "records_downloaded", "INTEGER DEFAULT 0"),
    ("job_items", "error_message", "VARCHAR"),
    ("job_items", "started_at", "TIMESTAMP"),
    ("job_items", "completed_at", "TIMESTAMP"),
    ("symbol_metadata", "expiry", "VARCHAR"),
    ("symbol_metadata", "strike", "DOUBLE"),
    ("symbol_metadata", "lotsize", "INTEGER"),
    ("symbol_metadata", "instrumenttype", "VARCHAR"),
    ("symbol_metadata", "tick_size", "DOUBLE"),
    ("symbol_metadata", "last_updated", "TIMESTAMP"),
    ("historify_schedules", "apscheduler_job_id", "VARCHAR"),
    ("historify_schedules", "next_run_at", "TIMESTAMP"),
    ("historify_schedules", "last_run_status", "VARCHAR"),
];

/// Tables whose ids come from sequences (web `ID_SEQUENCES`).
pub const ID_SEQUENCES: &[(&str, &str)] = &[
    ("watchlist_id_seq", "watchlist"),
    ("data_catalog_id_seq", "data_catalog"),
    ("job_items_id_seq", "job_items"),
    (
        "historify_schedule_executions_id_seq",
        "historify_schedule_executions",
    ),
];

pub fn table_exists(c: &Connection, table: &str) -> Result<bool> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM duckdb_tables() WHERE table_name = ? AND schema_name = 'main'",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn has_column(c: &Connection, table: &str, column: &str) -> Result<bool> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM duckdb_columns() WHERE table_name = ? AND column_name = ? \
         AND schema_name = 'main'",
        [table, column],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Bring the schema up to date. Safe to run on every open.
///
/// Runs with a zero checkpoint threshold, so every commit is checkpointed
/// straight into the file and nothing is left in the WAL. DuckDB 1.5.6
/// writes a WAL it cannot replay ("GetDefaultDatabase with no default
/// database set") when a table holding a FOREIGN KEY is dropped and the
/// referenced table has a non-constant default (`CURRENT_TIMESTAMP`): the
/// desktop-v1 `job_items` -> `download_jobs` pair rebuilt by `006`. A crash
/// before the next checkpoint left the store unopenable. A checkpoint on
/// commit is atomic: a crash leaves either the old file (the steps rerun)
/// or the migrated one.
pub fn run(c: &Connection) -> Result<()> {
    let previous: String =
        c.query_row("SELECT current_setting('checkpoint_threshold')", [], |r| {
            r.get(0)
        })?;
    c.execute_batch("SET checkpoint_threshold = '0b'")?;
    let out = run_steps(c);
    let restored = c.execute_batch(&format!(
        "SET checkpoint_threshold = '{}'",
        previous.replace('\'', "''")
    ));
    out?;
    restored?;
    if let Err(e) = c.execute_batch("CHECKPOINT") {
        tracing::warn!("Historify checkpoint after migrations failed: {}", e);
    }
    Ok(())
}

fn run_steps(c: &Connection) -> Result<()> {
    ensure_tracking(c)?;
    if !applied(c, "006_desktop_v1_to_web")? {
        in_tx(c, convert_desktop_v1)?;
        mark(c, "006_desktop_v1_to_web")?;
    }
    in_tx(c, ensure_web_schema)?;
    for change in sync_id_sequences(c, true)? {
        tracing::info!(
            "Historify ID sequence {} {}: next id {}",
            change.sequence,
            change.action,
            change.next_after
        );
    }
    Ok(())
}

fn in_tx(c: &Connection, f: impl FnOnce(&Connection) -> Result<()>) -> Result<()> {
    c.execute_batch("BEGIN TRANSACTION")?;
    match f(c) {
        Ok(()) => {
            c.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = c.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn ensure_tracking(c: &Connection) -> Result<()> {
    // The first desktop builds keyed this table by an integer id.
    if has_column(c, "migrations", "id")? {
        c.execute_batch("DROP TABLE migrations")?;
    }
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS migrations (
            name VARCHAR PRIMARY KEY,
            applied_at TIMESTAMP DEFAULT current_timestamp
        )",
    )?;
    Ok(())
}

fn applied(c: &Connection, name: &str) -> Result<bool> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM migrations WHERE name = ?",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn mark(c: &Connection, name: &str) -> Result<()> {
    c.execute(
        "INSERT INTO migrations (name) VALUES (?) ON CONFLICT DO NOTHING",
        [name],
    )?;
    Ok(())
}

fn ensure_web_schema(c: &Connection) -> Result<()> {
    c.execute_batch(WEB_TABLES)?;
    for (table, column, ty) in WEB_COLUMNS {
        if !has_column(c, table, column)? {
            c.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {} {}",
                table, column, ty
            ))?;
        }
    }
    c.execute_batch(WEB_INDEXES)?;
    Ok(())
}

/// Rebuild `table` from `select` (which reads the old table) in the web's
/// shape: create the new table under a temporary name, copy, drop the old
/// one, rename.
fn rebuild(c: &Connection, table: &str, create_new: &str, insert: &str) -> Result<()> {
    let tmp = format!("{}__web", table);
    c.execute_batch(&format!("DROP TABLE IF EXISTS {}", tmp))?;
    c.execute_batch(&create_new.replace("{T}", &tmp))?;
    c.execute_batch(&insert.replace("{T}", &tmp))?;
    c.execute_batch(&format!("DROP TABLE {}", table))?;
    c.execute_batch(&format!("ALTER TABLE {} RENAME TO {}", tmp, table))?;
    Ok(())
}

fn convert_desktop_v1(c: &Connection) -> Result<()> {
    // Old indexes depend on the tables being replaced.
    c.execute_batch(
        "DROP INDEX IF EXISTS idx_market_data_symbol;
         DROP INDEX IF EXISTS idx_market_data_timestamp;",
    )?;

    let old_market = table_exists(c, "market_data")? && has_column(c, "market_data", "timeframe")?;
    if old_market {
        rebuild(
            c,
            "market_data",
            "CREATE TABLE {T} (
                symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL, interval VARCHAR NOT NULL,
                timestamp BIGINT NOT NULL, open DOUBLE NOT NULL, high DOUBLE NOT NULL,
                low DOUBLE NOT NULL, close DOUBLE NOT NULL, volume BIGINT NOT NULL,
                oi BIGINT DEFAULT 0, created_at TIMESTAMP DEFAULT current_timestamp,
                PRIMARY KEY (symbol, exchange, interval, timestamp))",
            "INSERT INTO {T} (symbol, exchange, interval, timestamp, open, high, low, close, volume, oi)
             SELECT upper(symbol), upper(exchange),
                    CASE WHEN lower(timeframe) IN ('d', '1d', 'day', 'daily') THEN 'D'
                         WHEN lower(timeframe) IN ('1min', 'minute', '1minute') THEN '1m'
                         ELSE timeframe END,
                    epoch_ms(timestamp) // 1000, open, high, low, close, volume, 0
             FROM market_data
             ON CONFLICT DO NOTHING",
        )?;
    }

    if table_exists(c, "watchlist")? && has_column(c, "watchlist", "list_name")? {
        rebuild(
            c,
            "watchlist",
            "CREATE TABLE {T} (
                id INTEGER PRIMARY KEY, symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL,
                display_name VARCHAR, added_at TIMESTAMP DEFAULT current_timestamp,
                UNIQUE (symbol, exchange))",
            "INSERT INTO {T} (id, symbol, exchange, display_name, added_at)
             SELECT min(id), upper(symbol), upper(exchange),
                    first(name ORDER BY id), min(created_at)
             FROM watchlist GROUP BY upper(symbol), upper(exchange)",
        )?;
    }

    if table_exists(c, "job_items")? && has_column(c, "job_items", "timeframe")? {
        rebuild(
            c,
            "job_items",
            "CREATE TABLE {T} (
                id INTEGER PRIMARY KEY, job_id VARCHAR NOT NULL, symbol VARCHAR NOT NULL,
                exchange VARCHAR NOT NULL, status VARCHAR NOT NULL,
                records_downloaded INTEGER DEFAULT 0, error_message VARCHAR,
                started_at TIMESTAMP, completed_at TIMESTAMP)",
            "INSERT INTO {T} (id, job_id, symbol, exchange, status, error_message)
             SELECT id, CAST(job_id AS VARCHAR), upper(symbol), upper(exchange),
                    CASE WHEN status IN ('completed', 'done') THEN 'success'
                         WHEN status = 'failed' THEN 'error' ELSE status END,
                    error
             FROM job_items",
        )?;
    }

    if table_exists(c, "download_jobs")? && has_column(c, "download_jobs", "total_items")? {
        rebuild(
            c,
            "download_jobs",
            "CREATE TABLE {T} (
                id VARCHAR PRIMARY KEY, job_type VARCHAR NOT NULL, status VARCHAR NOT NULL,
                total_symbols INTEGER DEFAULT 0, completed_symbols INTEGER DEFAULT 0,
                failed_symbols INTEGER DEFAULT 0, interval VARCHAR, start_date VARCHAR,
                end_date VARCHAR, config VARCHAR,
                created_at TIMESTAMP DEFAULT current_timestamp, started_at TIMESTAMP,
                completed_at TIMESTAMP, error_message VARCHAR)",
            "INSERT INTO {T} (id, job_type, status, total_symbols, completed_symbols,
                              failed_symbols, created_at, completed_at)
             SELECT CAST(id AS VARCHAR), 'custom',
                    CASE WHEN status IN ('running', 'pending') THEN 'failed' ELSE status END,
                    total_items, completed_items, 0, created_at, completed_at
             FROM download_jobs",
        )?;
    }

    if table_exists(c, "symbol_metadata")? && has_column(c, "symbol_metadata", "sector")? {
        rebuild(
            c,
            "symbol_metadata",
            "CREATE TABLE {T} (
                symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL, name VARCHAR,
                expiry VARCHAR, strike DOUBLE, lotsize INTEGER, instrumenttype VARCHAR,
                tick_size DOUBLE, last_updated TIMESTAMP DEFAULT current_timestamp,
                PRIMARY KEY (symbol, exchange))",
            "INSERT INTO {T} (symbol, exchange, name, last_updated)
             SELECT upper(symbol), upper(exchange), name, updated_at FROM symbol_metadata
             ON CONFLICT DO NOTHING",
        )?;
    }

    if table_exists(c, "data_catalog")? && has_column(c, "data_catalog", "timeframe")? {
        // The catalog is derived data: rebuild it from the candles, keeping
        // the old download time where one was recorded.
        rebuild(
            c,
            "data_catalog",
            "CREATE TABLE {T} (
                id INTEGER PRIMARY KEY, symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL,
                interval VARCHAR NOT NULL, first_timestamp BIGINT, last_timestamp BIGINT,
                record_count BIGINT DEFAULT 0, last_download_at TIMESTAMP,
                UNIQUE (symbol, exchange, interval))",
            "INSERT INTO {T} (id, symbol, exchange, interval, first_timestamp, last_timestamp,
                              record_count, last_download_at)
             SELECT row_number() OVER (ORDER BY m.symbol, m.exchange, m.interval),
                    m.symbol, m.exchange, m.interval, m.f, m.l, m.n, o.last_updated
             FROM (SELECT symbol, exchange, interval, MIN(timestamp) AS f,
                          MAX(timestamp) AS l, COUNT(*) AS n
                   FROM market_data GROUP BY symbol, exchange, interval) m
             LEFT JOIN (SELECT upper(symbol) AS symbol, upper(exchange) AS exchange,
                               max(last_updated) AS last_updated
                        FROM data_catalog GROUP BY 1, 2) o
               ON o.symbol = m.symbol AND o.exchange = m.exchange",
        )?;
    } else if old_market && table_exists(c, "data_catalog")? {
        // Unreachable in practice (both came from the same release), but a
        // converted market table must never keep a stale catalog.
        c.execute_batch("DELETE FROM data_catalog")?;
    }
    Ok(())
}

/// One sequence that needed work.
#[derive(Debug, Clone, PartialEq)]
pub struct SequenceChange {
    pub sequence: &'static str,
    pub action: &'static str,
    pub next_before: Option<i64>,
    pub next_after: i64,
}

/// Web `_next_sequence_value`.
fn next_sequence_value(start: i64, last: Option<i64>, step: i64) -> i64 {
    match last {
        None => start,
        Some(l) if l == start => start,
        Some(l) => l + step,
    }
}

/// Web `sync_id_sequences`: create a missing sequence at its table's
/// high-water mark, move one that fell behind past it, leave the rest.
pub fn sync_id_sequences(c: &Connection, apply: bool) -> Result<Vec<SequenceChange>> {
    let mut out = Vec::new();
    for (sequence, table) in ID_SEQUENCES {
        if !table_exists(c, table)? {
            continue;
        }
        let next_id: i64 = c.query_row(
            &format!(
                "SELECT CAST(COALESCE(MAX(id), 0) + 1 AS BIGINT) FROM {}",
                table
            ),
            [],
            |r| r.get(0),
        )?;
        let row: Option<(i64, Option<i64>, i64)> = {
            let mut st = c.prepare(
                "SELECT start_value, last_value, increment_by FROM duckdb_sequences() \
                 WHERE sequence_name = ?",
            )?;
            let mut rows = st.query([sequence])?;
            match rows.next()? {
                Some(r) => Some((r.get(0)?, r.get(1)?, r.get(2)?)),
                None => None,
            }
        };
        let (action, current) = match row {
            None => ("create", None),
            Some((start, last, step)) => {
                let cur = next_sequence_value(start, last, step);
                if cur >= next_id {
                    continue;
                }
                ("advance", Some(cur))
            }
        };
        if apply {
            let verb = if action == "create" {
                "CREATE SEQUENCE"
            } else {
                "CREATE OR REPLACE SEQUENCE"
            };
            c.execute_batch(&format!("{} {} START {}", verb, sequence, next_id))?;
        }
        out.push(SequenceChange {
            sequence,
            action,
            next_before: current,
            next_after: next_id,
        });
    }
    Ok(out)
}

/// Draw the next id from a sequence.
pub fn next_id(c: &Connection, sequence: &str) -> Result<i64> {
    if !ID_SEQUENCES.iter().any(|(s, _)| *s == sequence) {
        return Err(AppError::Internal(format!("unknown sequence {}", sequence)));
    }
    Ok(c.query_row(&format!("SELECT nextval('{}')", sequence), [], |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_database_gets_every_web_table_and_runs_twice() {
        let c = Connection::open_in_memory().unwrap();
        run(&c).unwrap();
        run(&c).unwrap();
        for t in [
            "market_data",
            "watchlist",
            "data_catalog",
            "download_jobs",
            "job_items",
            "symbol_metadata",
            "historify_schedules",
            "historify_schedule_executions",
        ] {
            assert!(table_exists(&c, t).unwrap(), "{}", t);
        }
        assert!(has_column(&c, "market_data", "interval").unwrap());
        assert_eq!(next_id(&c, "watchlist_id_seq").unwrap(), 1);
    }

    #[test]
    fn sequence_value_reading_matches_the_web() {
        assert_eq!(next_sequence_value(5, None, 1), 5);
        assert_eq!(next_sequence_value(5, Some(5), 1), 5);
        assert_eq!(next_sequence_value(5, Some(9), 1), 10);
    }

    #[test]
    fn a_sequence_behind_its_table_is_advanced() {
        let c = Connection::open_in_memory().unwrap();
        run(&c).unwrap();
        c.execute_batch(
            "INSERT INTO watchlist (id, symbol, exchange) VALUES (41, 'A', 'NSE'), (42, 'B', 'NSE')",
        )
        .unwrap();
        let changes = sync_id_sequences(&c, true).unwrap();
        assert!(changes
            .iter()
            .any(|ch| ch.sequence == "watchlist_id_seq" && ch.action == "advance"));
        assert_eq!(next_id(&c, "watchlist_id_seq").unwrap(), 43);
    }
}
