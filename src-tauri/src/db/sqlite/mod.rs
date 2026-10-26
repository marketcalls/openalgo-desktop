//! SQLite database module (main database, `openalgo.db`).
//!
//! All access goes through an r2d2 pool. A connection is checked out for the
//! duration of one synchronous call and returned when it drops; no method
//! holds a connection across an `.await`.

pub mod action_center;
pub mod alert_log;
pub mod api_keys;
pub mod auth;
pub mod chart_prefs;
pub mod credentials;
pub mod data_migrations;
mod durability;
pub mod key_vault;
pub mod logs;
pub mod market_calendar;
pub mod master_contract_status;
mod master_swap;
pub(crate) mod migrations;
pub mod models;
pub mod monitor;
pub mod oauth_state;
pub mod rate_plan;
pub mod request_count;
mod settings;
pub mod strategy_portfolio;
pub mod symbol;
pub mod tunnel;
pub mod user;
pub mod watchlist;
pub mod webui;

use crate::error::Result;
pub use durability::Durability;
use models::Settings;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use std::path::Path;
use std::time::Duration;

pub type DbConn = PooledConnection<SqliteConnectionManager>;

/// Build a pool with the pragmas every connection needs, at the store's
/// [`Durability`], applied to each connection the pool opens.
pub fn open_pool(
    path: &Path,
    max_size: u32,
    durability: Durability,
) -> Result<Pool<SqliteConnectionManager>> {
    let manager = SqliteConnectionManager::file(path).with_init(move |c| {
        c.execute_batch("PRAGMA journal_mode=WAL;")?;
        c.execute_batch(durability.pragma())?;
        c.execute_batch("PRAGMA foreign_keys=ON;")?;
        c.busy_handler(Some(retry_busy))
    });
    let pool = Pool::builder()
        .max_size(max_size)
        .min_idle(Some(1))
        .connection_timeout(Duration::from_secs(10))
        .idle_timeout(Some(Duration::from_secs(300)))
        .build(manager)?;
    crate::security::fsperm::restrict_db_files(path)?;
    Ok(pool)
}

/// Open a transaction that writes: `BEGIN IMMEDIATE`, which takes the
/// write lock at the start, where the busy handler (`retry_busy`) waits
/// for another writer. A deferred transaction (`Connection::transaction`)
/// that reads first takes the lock only at its first write, and SQLite
/// answers that upgrade from a read "database is locked" at once, without
/// calling the busy handler: a scalping stop saved while another writer
/// held `openalgo.db` was refused that way (found by `tests/it/perf`).
/// `transaction_guard` keeps deferred transactions out of the code.
pub(crate) fn write_transaction(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
}

/// How long a statement waits for another writer before it fails busy.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The busy handler of every pooled connection: try again every
/// millisecond (every timer tick where sleeps are coarser), up to
/// [`BUSY_TIMEOUT`] in all. SQLite's own (`busy_timeout`) backs off to
/// 100 ms between tries, so a write waiting behind a series of short
/// transactions, the master replacement's (`master_swap`), kept missing
/// the pauses between them (DB-01). `attempt` is 0 on the first call of
/// each wait, on the thread running the statement.
fn retry_busy(attempt: i32) -> bool {
    thread_local! {
        static WAITING_SINCE: std::cell::Cell<Option<std::time::Instant>> =
            const { std::cell::Cell::new(None) };
    }
    let now = std::time::Instant::now();
    let since = WAITING_SINCE.with(|s| {
        if attempt == 0 || s.get().is_none() {
            s.set(Some(now));
        }
        s.get().unwrap_or(now)
    });
    if now.duration_since(since) >= BUSY_TIMEOUT {
        return false;
    }
    std::thread::sleep(Duration::from_millis(1));
    true
}

/// SQLite database wrapper
pub struct SqliteDb {
    pool: Pool<SqliteConnectionManager>,
    /// Analyzer mode as last read or written here, its only writer. Every
    /// order path routes on it, so it is kept in memory: a busy pool must not
    /// turn the read into a default. `None` until the database has answered.
    analyze_mode: parking_lot::Mutex<Option<bool>>,
}

impl SqliteDb {
    /// Open (creating if needed) the main database and run schema migrations.
    pub fn new(path: &Path) -> Result<Self> {
        let db = Self {
            pool: open_pool(path, 8, Durability::MAIN)?,
            analyze_mode: parking_lot::Mutex::new(None),
        };
        db.run_migrations()?;
        crate::security::fsperm::restrict_db_files(path)?;
        // Read once now, so no order waits on the pool for it later. A
        // failure leaves it unknown, and order paths refuse until it is read.
        if let Err(e) = db.get_analyze_mode() {
            tracing::error!("Could not read analyzer mode at start-up: {}", e);
        }
        Ok(db)
    }

    /// Check out a pooled connection. Drop it before any network await.
    pub fn conn(&self) -> Result<DbConn> {
        Ok(self.pool.get()?)
    }

    /// Open and idle pooled connections (health monitor).
    pub fn pool_state(&self) -> (u32, u32) {
        let st = self.pool.state();
        (st.connections, st.idle_connections)
    }

    /// Run database migrations
    fn run_migrations(&self) -> Result<()> {
        let conn = self.conn()?;
        migrations::run_migrations(&conn)
    }

    // ========== Settings Methods ==========

    /// Get settings
    pub fn get_settings(&self) -> Result<Settings> {
        let conn = self.conn()?;
        settings::get_settings(&conn)
    }

    // ========== Analyze Mode Methods ==========

    /// Whether analyzer (sandbox) mode is on: the value held in memory, read
    /// from the database only until it has answered once. An error means it
    /// has never been readable, and callers that route orders refuse on it.
    pub fn get_analyze_mode(&self) -> Result<bool> {
        let mut known = self.analyze_mode.lock();
        if let Some(on) = *known {
            return Ok(on);
        }
        let on = self.get_settings()?.analyze_mode.unwrap_or(false);
        *known = Some(on);
        Ok(on)
    }

    /// Set analyzer mode. The write and the value in memory change under one
    /// lock, after the database has taken it, so a reader never sees a mode
    /// the database does not hold.
    pub fn set_analyze_mode(&self, enabled: bool) -> Result<()> {
        let mut known = self.analyze_mode.lock();
        let conn = self.conn()?;
        conn.execute("UPDATE settings SET analyze_mode = ?1", [enabled])?;
        *known = Some(enabled);
        Ok(())
    }
}

#[cfg(test)]
mod transaction_guard;

#[cfg(test)]
mod tests {
    use super::*;

    /// DB-01: a write waiting behind a transaction gets in during the
    /// master replacement's pause that follows it, every time. Checked by
    /// order, not by the clock: the holder counts the rows at the start of
    /// each transaction, so a write that missed the pause shows up as a
    /// count one short, however slow the runner. SQLite's own
    /// `busy_timeout` retries at about 228, 328 and 428 ms into its wait:
    /// behind a 350 ms transaction none of its tries fall in the pause, so
    /// it misses every time; `retry_busy` tries every millisecond (every
    /// timer tick where sleeps are coarser) and gets in each time.
    #[test]
    fn a_waiting_write_gets_in_at_the_first_pause() {
        let dir = tempfile::tempdir().unwrap();
        let pool = open_pool(&dir.path().join("t.db"), 2, Durability::MAIN).unwrap();
        let holder = pool.get().unwrap();
        holder.execute_batch("CREATE TABLE t (v INTEGER)").unwrap();
        let hold = Duration::from_millis(350);
        let rounds = 3;
        let (began, holding) = std::sync::mpsc::sync_channel(rounds);
        let busy = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for round in 0..=rounds {
                holder.execute_batch("BEGIN IMMEDIATE").unwrap();
                let rows: i64 = holder
                    .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
                    .unwrap();
                seen.push(rows);
                if round == rounds {
                    holder.execute_batch("COMMIT").unwrap();
                    break;
                }
                let _ = began.try_send(());
                std::thread::sleep(hold);
                holder.execute_batch("COMMIT").unwrap();
                std::thread::sleep(master_swap::PAUSE);
            }
            seen
        });
        let writer = pool.get().unwrap();
        for _ in 0..rounds {
            holding.recv().unwrap();
            writer.execute("INSERT INTO t VALUES (1)", []).unwrap();
        }
        let seen = busy.join().unwrap();
        // Before round k's transaction the writer's k earlier writes are in:
        // each got in during the pause right after the transaction it waited
        // for, ahead of the holder's next one.
        assert_eq!(seen, vec![0, 1, 2, 3], "rows seen at each transaction");
    }
}
