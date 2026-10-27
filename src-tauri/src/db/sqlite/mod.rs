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
pub mod user;
pub mod watchlist;
pub mod webui;

use crate::error::Result;
pub use durability::Durability;
use models::Settings;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub type DbConn = PooledConnection<SqliteConnectionManager>;

thread_local! {
    /// Pooled connections this thread has taken, and how long it waited
    /// for them ([`checkouts`]).
    static CHECKOUTS: std::cell::Cell<(u32, Duration)> =
        const { std::cell::Cell::new((0, Duration::ZERO)) };
}

/// Take a connection from `pool`, counted against this thread.
pub(crate) fn checkout(pool: &Pool<SqliteConnectionManager>) -> Result<DbConn> {
    let started = std::time::Instant::now();
    let conn = pool.get();
    let waited = started.elapsed();
    CHECKOUTS.with(|c| {
        let (n, w) = c.get();
        c.set((n.wrapping_add(1), w.saturating_add(waited)));
    });
    Ok(conn?)
}

/// Pooled connections taken on this thread so far, and the time it spent
/// waiting for them. A pool with no free connection holds the thread for
/// the whole wait, so a test on a current-thread runtime reads it to prove
/// a request never took one on the runtime that carries the feed and the
/// risk owners (`tests/it/polled_reads.rs`).
pub fn checkouts() -> (u32, Duration) {
    CHECKOUTS.with(std::cell::Cell::get)
}

/// How long a checkout waits for a pooled connection.
const POOL_WAIT: Duration = Duration::from_secs(10);

/// How long opening a store waits, in all, for its first connection while
/// it is still being opened. A machine that stalls at the app's start (a
/// macOS CI runner froze every process for 10-16 s, twice in one run) or
/// a disk scanned or busy then made the checkout deadline fail the start
/// with `Pool(Error(None))`, although nothing had failed. A connection
/// that fails still fails the open at the first deadline, with its error.
const FIRST_CONNECTION_LIMIT: Duration = Duration::from_secs(120);

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
    let pool = build_pool(manager, max_size, POOL_WAIT, FIRST_CONNECTION_LIMIT)?;
    crate::security::fsperm::restrict_db_files(path)?;
    Ok(pool)
}

/// Why a connection the pool tried to open failed: logged, and kept for
/// [`build_pool`]'s wait for the first one.
#[derive(Debug)]
struct ConnectFailure(Arc<parking_lot::Mutex<Option<rusqlite::Error>>>);

impl r2d2::HandleError<rusqlite::Error> for ConnectFailure {
    fn handle_error(&self, error: rusqlite::Error) {
        tracing::error!("A database connection could not be opened: {}", error);
        *self.0.lock() = Some(error);
    }
}

/// The pool, once its first connection is open. Every checkout waits at
/// most `wait`; the first connection is waited for while it is being
/// opened, up to `limit`, and a failure to open it ends the wait.
fn build_pool(
    manager: SqliteConnectionManager,
    max_size: u32,
    wait: Duration,
    limit: Duration,
) -> Result<Pool<SqliteConnectionManager>> {
    let failure = Arc::new(parking_lot::Mutex::new(None));
    let pool = Pool::builder()
        .max_size(max_size)
        .min_idle(Some(1))
        .connection_timeout(wait)
        .idle_timeout(Some(Duration::from_secs(300)))
        .error_handler(Box::new(ConnectFailure(failure.clone())))
        .build_unchecked(manager);
    let started = std::time::Instant::now();
    loop {
        let timed_out = match pool.get_timeout(wait) {
            Ok(_) => return Ok(pool),
            Err(e) => e,
        };
        if let Some(cause) = failure.lock().take() {
            return Err(cause.into());
        }
        if started.elapsed() >= limit {
            return Err(timed_out.into());
        }
        tracing::warn!(
            "Opening a database is taking longer than usual ({} s so far); still waiting",
            started.elapsed().as_secs()
        );
    }
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
        checkout(&self.pool)
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

    /// CI flake b: a store whose first connection takes longer to open
    /// than a checkout may wait is opened, not refused. On a stalled macOS
    /// runner the app's start failed with `Pool(Error(None))`: the
    /// connection was still opening, nothing had failed.
    #[test]
    fn a_first_connection_slower_than_a_checkout_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let manager = SqliteConnectionManager::file(dir.path().join("slow.db")).with_init(|_| {
            std::thread::sleep(Duration::from_millis(300));
            Ok(())
        });
        let pool = build_pool(
            manager,
            2,
            Duration::from_millis(50),
            Duration::from_secs(60),
        );
        assert!(pool.is_ok(), "{:?}", pool.err());
    }

    /// A first connection that fails ends the wait at the first checkout
    /// deadline, with SQLite's reason, never only at the open's limit.
    #[test]
    fn a_first_connection_that_fails_fails_the_open_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let manager = SqliteConnectionManager::file(dir.path().join("missing").join("x.db"));
        let started = std::time::Instant::now();
        let refused = build_pool(
            manager,
            2,
            Duration::from_millis(50),
            Duration::from_secs(60),
        );
        let Err(e) = refused else {
            panic!("a database in a folder that does not exist opened");
        };
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert!(e.to_string().contains("unable to open"), "{}", e);
    }

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
