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
    switch_to_wal(path)?;
    let manager =
        SqliteConnectionManager::file(path).with_init(move |c| init_connection(c, durability));
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pool = build_pool(manager, file, max_size, POOL_WAIT, FIRST_CONNECTION_LIMIT)?;
    crate::security::fsperm::restrict_db_files(path)?;
    Ok(pool)
}

/// Put the file in WAL mode, on one connection, before the pool opens any.
/// A pool opens its first two connections at once on its own threads (one
/// for `min_idle`, one for the first checkout); on a new file both switched
/// it to WAL, and SQLite refuses one of them at once, without a busy wait
/// (each held the read lock the other needed gone): "database is locked" on
/// every start with a new data folder. The mode is kept in the file, so a
/// pooled connection's own pragma then only reads it.
fn switch_to_wal(path: &Path) -> Result<()> {
    let c = rusqlite::Connection::open(path)?;
    c.busy_handler(Some(retry_busy))?;
    c.execute_batch("PRAGMA journal_mode=WAL;")?;
    Ok(())
}

/// Every pooled connection's set-up, its busy handler first so the set-up
/// waits for a held file the way every later statement does.
fn init_connection(c: &mut rusqlite::Connection, durability: Durability) -> rusqlite::Result<()> {
    c.busy_handler(Some(retry_busy))?;
    c.execute_batch("PRAGMA journal_mode=WAL;")?;
    c.execute_batch(durability.pragma())?;
    c.execute_batch("PRAGMA foreign_keys=ON;")
}

/// The files a connection failed to open on in this test process.
#[cfg(test)]
static FAILED_OPENS: parking_lot::Mutex<Vec<String>> = parking_lot::Mutex::new(Vec::new());

/// Why a connection the pool tried to open failed: logged with its file,
/// and kept for [`build_pool`]'s wait for the first one.
#[derive(Debug)]
struct ConnectFailure {
    why: Arc<parking_lot::Mutex<Option<rusqlite::Error>>>,
    file: String,
}

impl r2d2::HandleError<rusqlite::Error> for ConnectFailure {
    fn handle_error(&self, error: rusqlite::Error) {
        #[cfg(test)]
        FAILED_OPENS
            .lock()
            .push(format!("{}: {}", self.file, error));
        tracing::error!(
            "A database connection could not be opened ({}): {}",
            self.file,
            error
        );
        *self.why.lock() = Some(error);
    }
}

/// The threads a pool opens connections and reaps idle ones on, ended
/// with the pool. r2d2's own run every job still scheduled first, and the
/// idle reaper is always scheduled (every 30 s), so each pool dropped left
/// its three threads behind for up to 30 s: a test process that opened and
/// dropped app contexts quickly (31 in one test) reached macOS's 2,048
/// threads, and r2d2 could not start its next pool's threads.
fn workers() -> Arc<scheduled_thread_pool::ScheduledThreadPool> {
    Arc::new(
        scheduled_thread_pool::ScheduledThreadPool::builder()
            .num_threads(3)
            .thread_name_pattern("r2d2-worker-{}")
            .on_drop_behavior(scheduled_thread_pool::OnPoolDropBehavior::DiscardPendingScheduled)
            .build(),
    )
}

/// The pool, once its first connection is open. Every checkout waits at
/// most `wait`; the first connection is waited for while it is being
/// opened, up to `limit`, and a failure to open it ends the wait.
fn build_pool(
    manager: SqliteConnectionManager,
    file: String,
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
        .error_handler(Box::new(ConnectFailure {
            why: failure.clone(),
            file,
        }))
        .thread_pool(workers())
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

#[cfg(test)]
thread_local! {
    /// The longest a statement on this thread has waited for another
    /// writer's lock, since [`take_lock_wait`] last read it.
    static LOCK_WAIT: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
}

/// How long this thread's statements waited for another writer's lock at
/// most, since the last call: the wait alone, without the statement's own
/// work or its commit.
#[cfg(test)]
pub(crate) fn take_lock_wait() -> Duration {
    LOCK_WAIT.with(|w| w.replace(Duration::ZERO))
}

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
    #[cfg(test)]
    LOCK_WAIT.with(|w| w.set(w.get().max(now.duration_since(since))));
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
            "slow.db".into(),
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
            "x.db".into(),
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

    /// The opens refused because another connection held the file, as
    /// `file: reason`.
    fn locked_opens() -> Vec<String> {
        FAILED_OPENS
            .lock()
            .iter()
            .filter(|f| f.contains("locked") || f.contains("busy"))
            .cloned()
            .collect()
    }

    /// A pool on a new file opens its first two connections at once; both
    /// switched the file to WAL and SQLite refused one, without a wait.
    #[test]
    fn a_new_file_opens_its_first_connections_first_time() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<String> = (0..20).map(|i| format!("new-{}.db", i)).collect();
        for f in &files {
            let pool = open_pool(&dir.path().join(f), 4, Durability::MAIN).unwrap();
            let c = pool.get().unwrap();
            let mode: String = c
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            // The pool's second connection has finished opening.
            let _second = pool.get().unwrap();
        }
        let ours: Vec<String> = locked_opens()
            .into_iter()
            .filter(|f| files.iter().any(|n| f.starts_with(&format!("{}: ", n))))
            .collect();
        assert_eq!(ours, Vec::<String>::new());
    }

    /// The app's start on a new data folder opens every database
    /// connection the first time (the e2e lane saw "database is locked"
    /// logged on every start on a temporary folder).
    #[tokio::test]
    async fn a_fresh_app_start_opens_every_connection_first_time() {
        let mut files = Vec::new();
        for _ in 0..5 {
            let t = crate::state::testing::build(
                crate::brokers::BrokerRegistry::new(),
                chrono::Utc::now(),
            );
            assert!(t.ctx.sqlite.get_settings().is_ok());
            for e in std::fs::read_dir(t.dir.path()).unwrap().flatten() {
                files.push(e.file_name().to_string_lossy().into_owned());
            }
        }
        assert_eq!(locked_opens(), Vec::<String>::new(), "files: {:?}", files);
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
