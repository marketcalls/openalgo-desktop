//! The Historify DuckDB store: one database instance per process, handed out
//! as short-lived connections.
//!
//! DuckDB takes an exclusive lock on its file, so the process opens the file
//! exactly once (the root connection) and every operation borrows a clone of
//! it (`try_clone` shares the database instance). A borrowed connection is a
//! [`DuckConn`] guard: it closes when dropped, on every path, and the live
//! count is observable so tests can prove nothing is left open after a job is
//! cancelled or the app shuts down.
//!
//! DuckDB resolves conflicting writes optimistically (one transaction loses),
//! so every logical mutation runs under one in-process write lock, like the
//! web's `_historify_write_lock`. Reads run concurrently.
//!
//! Every call here blocks; async callers go through [`HistorifyDb::run`] or
//! [`HistorifyDb::write`], which move the work onto the blocking pool.
//!
//! Unclean exits: DuckDB keeps committed work in `historify.duckdb.wal`
//! until a checkpoint folds it into the file. Some WAL records cannot be
//! replayed by DuckDB 1.5.6 (see `migrations::run`), and one such record
//! made the store unopenable. So the migrations never leave a WAL behind,
//! [`HistorifyDb::close`] checkpoints, and an open that fails while
//! replaying the WAL moves the WAL aside (never deleting it) and opens the
//! last checkpointed state, reporting it through [`HistorifyDb::recovery`].

pub mod migrations;

use crate::error::{AppError, Result};
use duckdb::{Config, Connection};
use parking_lot::Mutex;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Trader-facing text when the store cannot be used.
pub const STORE_UNAVAILABLE: &str =
    "The Historify database is not available. Restart OpenAlgo and try again.";

/// Trader-facing text after an unreplayable WAL was moved aside.
pub const WAL_RECOVERED: &str = "OpenAlgo did not close cleanly last time, and some unsaved \
     Historify changes from that session could not be recovered. Historify has been reopened \
     with the data saved before then. Check your Historify watchlist and download any recent \
     data again if it is missing.";

/// The marker DuckDB puts in the error when replaying the WAL fails.
const WAL_REPLAY_ERROR: &str = "Failure while replaying WAL file";

/// An unreplayable WAL found at open and moved aside.
#[derive(Debug, Clone, PartialEq)]
pub struct WalRecovery {
    /// Where the WAL now lives (kept for diagnosis, never deleted).
    pub moved_to: PathBuf,
    /// DuckDB's reason for refusing it.
    pub reason: String,
}

struct Inner {
    root: Mutex<Option<Connection>>,
    /// Set by [`HistorifyDb::seal`]: no new borrows, root still open.
    sealed: AtomicBool,
    write_lock: Mutex<()>,
    live: AtomicUsize,
    path: PathBuf,
    recovery: Option<WalRecovery>,
}

/// Shared handle to the Historify database.
#[derive(Clone)]
pub struct HistorifyDb {
    inner: Arc<Inner>,
}

/// Back-compatible name used by the application context.
pub type DuckDb = HistorifyDb;

/// A borrowed connection; closed when dropped.
pub struct DuckConn {
    conn: Option<Connection>,
    inner: Arc<Inner>,
}

impl Deref for DuckConn {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        // Present from construction until drop.
        self.conn.as_ref().unwrap_or_else(|| unreachable!())
    }
}

impl DerefMut for DuckConn {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().unwrap_or_else(|| unreachable!())
    }
}

impl Drop for DuckConn {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            if let Err((_, e)) = c.close() {
                tracing::debug!("Closing a Historify connection reported: {}", e);
            }
        }
        self.inner.live.fetch_sub(1, Ordering::SeqCst);
    }
}

fn config() -> Result<Config> {
    // Never fetch extensions from the network; Parquet is compiled in.
    Ok(Config::default().enable_autoload_extension(false)?)
}

impl HistorifyDb {
    /// Open (or create) the database file and bring its schema up to date.
    /// A WAL that DuckDB cannot replay is moved aside first (see
    /// [`Self::recovery`]); any other open error is returned as is.
    pub fn new(path: &Path) -> Result<Self> {
        let (root, recovery) = open_root(path)?;
        let db = Self::from_root(root, path.to_path_buf(), recovery);
        {
            let c = db.conn()?;
            let _w = db.inner.write_lock.lock();
            migrations::run(&c)?;
        }
        Ok(db)
    }

    /// An in-memory store (unit tests).
    pub fn in_memory() -> Result<Self> {
        let root = Connection::open_in_memory_with_flags(config()?)?;
        let db = Self::from_root(root, PathBuf::new(), None);
        {
            let c = db.conn()?;
            migrations::run(&c)?;
        }
        Ok(db)
    }

    fn from_root(root: Connection, path: PathBuf, recovery: Option<WalRecovery>) -> Self {
        Self {
            inner: Arc::new(Inner {
                root: Mutex::new(Some(root)),
                sealed: AtomicBool::new(false),
                write_lock: Mutex::new(()),
                live: AtomicUsize::new(0),
                path,
                recovery,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Set when this open had to move an unreplayable WAL aside.
    pub fn recovery(&self) -> Option<&WalRecovery> {
        self.inner.recovery.as_ref()
    }

    /// Borrow a connection. Fails once the store is sealed or closed.
    pub fn conn(&self) -> Result<DuckConn> {
        let root = self.inner.root.lock();
        let Some(r) = root
            .as_ref()
            .filter(|_| !self.inner.sealed.load(Ordering::SeqCst))
        else {
            return Err(AppError::Internal("Historify store is closed".into()));
        };
        let c = r.try_clone()?;
        self.inner.live.fetch_add(1, Ordering::SeqCst);
        Ok(DuckConn {
            conn: Some(c),
            inner: self.inner.clone(),
        })
    }

    /// Run a read on a borrowed connection (blocking).
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let c = self.conn()?;
        f(&c)
    }

    /// Run one complete mutation under the write lock, in a transaction
    /// (blocking). A failure rolls the whole mutation back.
    pub fn mutate<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut c = self.conn()?;
        let _w = self.inner.write_lock.lock();
        let tx = c.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// [`Self::read`] on the blocking pool.
    pub async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.read(f))
            .await
            .map_err(|e| AppError::Internal(format!("Historify read task failed: {}", e)))?
    }

    /// [`Self::mutate`] on the blocking pool.
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.mutate(f))
            .await
            .map_err(|e| AppError::Internal(format!("Historify write task failed: {}", e)))?
    }

    /// Borrowed connections currently open (excluding the root).
    pub fn open_connections(&self) -> usize {
        self.inner.live.load(Ordering::SeqCst)
    }

    /// Refuse new borrows while keeping the root open, so shutdown can wait
    /// for the borrowed connections to drain before it checkpoints. Work
    /// already queued on the blocking pool (from a task aborted at shutdown)
    /// then fails to borrow instead of opening a connection after the wait.
    pub fn seal(&self) {
        let _root = self.inner.root.lock();
        self.inner.sealed.store(true, Ordering::SeqCst);
    }

    pub fn is_open(&self) -> bool {
        self.inner.root.lock().is_some()
    }

    /// Checkpoint, then close the root connection. The checkpoint folds the
    /// WAL into the file, so a crash after a clean close has nothing to
    /// replay. The file is released once every borrowed connection has been
    /// dropped; later borrows fail.
    pub fn close(&self) {
        let root = self.inner.root.lock().take();
        if let Some(r) = root {
            // DuckDB refuses (rather than waits for) a checkpoint while
            // another write is in flight; the WAL is then kept and replayed.
            if !self.inner.path.as_os_str().is_empty() {
                if let Err(e) = r.execute_batch("CHECKPOINT") {
                    tracing::warn!("Historify checkpoint at shutdown failed: {}", e);
                }
            }
            if let Err((_, e)) = r.close() {
                tracing::warn!("Closing the Historify database reported: {}", e);
            }
        }
    }
}

/// `<db>.wal`, where DuckDB keeps committed work not yet checkpointed.
pub fn wal_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".wal");
    PathBuf::from(p)
}

fn is_wal_replay_error(e: &duckdb::Error) -> bool {
    e.to_string().contains(WAL_REPLAY_ERROR)
}

/// Open the root connection. Only a WAL replay failure is recovered from:
/// the WAL is renamed to `<db>.wal.unreplayable-<UTC time>` and the open
/// retried once. Every other error is returned unchanged.
fn open_root(path: &Path) -> Result<(Connection, Option<WalRecovery>)> {
    let err = match Connection::open_with_flags(path, config()?) {
        Ok(c) => return Ok((c, None)),
        Err(e) => e,
    };
    let wal = wal_path(path);
    if !is_wal_replay_error(&err) || !wal.exists() {
        return Err(err.into());
    }
    let reason = err.to_string();
    let moved_to = aside_path(&wal);
    std::fs::rename(&wal, &moved_to)?;
    tracing::warn!(
        "Historify could not replay its write-ahead log after an unclean exit; moved it to {} \
         and opened the last checkpointed state. Reason: {}",
        moved_to.display(),
        reason
    );
    let c = Connection::open_with_flags(path, config()?)?;
    Ok((c, Some(WalRecovery { moved_to, reason })))
}

/// A name next to the WAL that no earlier recovery used.
fn aside_path(wal: &Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let base = format!("{}.unreplayable-{}", wal.display(), stamp);
    let mut candidate = PathBuf::from(&base);
    let mut n = 1;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{}-{}", base, n));
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_connections_close_on_drop_and_after_close() {
        let db = HistorifyDb::in_memory().unwrap();
        {
            let a = db.conn().unwrap();
            let _b = db.conn().unwrap();
            assert_eq!(db.open_connections(), 2);
            let n: i64 = a
                .query_row("SELECT COUNT(*) FROM watchlist", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0);
        }
        assert_eq!(db.open_connections(), 0);
        db.close();
        assert!(db.conn().is_err());
        assert_eq!(db.open_connections(), 0);
    }

    #[test]
    fn a_sealed_store_refuses_new_borrows_until_closed() {
        let db = HistorifyDb::in_memory().unwrap();
        let held = db.conn().unwrap();
        db.seal();
        assert!(db.conn().is_err());
        assert!(db.is_open());
        assert_eq!(db.open_connections(), 1);
        drop(held);
        assert_eq!(db.open_connections(), 0);
        db.close();
        assert!(!db.is_open());
    }

    #[test]
    fn failed_mutation_rolls_back() {
        let db = HistorifyDb::in_memory().unwrap();
        let r: Result<()> = db.mutate(|c| {
            c.execute(
                "INSERT INTO watchlist (id, symbol, exchange) VALUES (1, 'A', 'NSE')",
                [],
            )?;
            Err(AppError::Internal("boom".into()))
        });
        assert!(r.is_err());
        let n: i64 = db
            .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM watchlist", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(db.open_connections(), 0);
    }

    /// The tables the first desktop builds created (the shape of the file
    /// that was bricked): `job_items` references `download_jobs`, whose
    /// `created_at` has a `CURRENT_TIMESTAMP` default.
    const DESKTOP_V1: &str = "
        CREATE TABLE migrations (name VARCHAR PRIMARY KEY,
            applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP);
        INSERT INTO migrations (name) VALUES ('001_market_data'), ('002_watchlist'),
            ('003_data_catalog'), ('004_download_jobs'), ('005_symbol_metadata');
        CREATE TABLE market_data (symbol VARCHAR, exchange VARCHAR, timeframe VARCHAR,
            timestamp TIMESTAMP, open DOUBLE NOT NULL, high DOUBLE NOT NULL, low DOUBLE NOT NULL,
            close DOUBLE NOT NULL, volume BIGINT NOT NULL,
            PRIMARY KEY (symbol, exchange, timeframe, timestamp));
        CREATE INDEX idx_market_data_symbol ON market_data(symbol, exchange);
        CREATE INDEX idx_market_data_timestamp ON market_data(timestamp);
        CREATE TABLE watchlist (id INTEGER PRIMARY KEY, symbol VARCHAR NOT NULL,
            exchange VARCHAR NOT NULL, name VARCHAR NOT NULL,
            list_name VARCHAR NOT NULL DEFAULT 'default', order_index INTEGER NOT NULL DEFAULT 0,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, UNIQUE (symbol, exchange, list_name));
        CREATE TABLE data_catalog (id INTEGER PRIMARY KEY, symbol VARCHAR NOT NULL,
            exchange VARCHAR NOT NULL, timeframe VARCHAR NOT NULL, from_date DATE NOT NULL,
            to_date DATE NOT NULL, row_count BIGINT NOT NULL DEFAULT 0,
            last_updated TIMESTAMP DEFAULT CURRENT_TIMESTAMP, UNIQUE (symbol, exchange, timeframe));
        CREATE TABLE download_jobs (id INTEGER PRIMARY KEY, name VARCHAR NOT NULL,
            status VARCHAR NOT NULL DEFAULT 'pending', total_items INTEGER NOT NULL DEFAULT 0,
            completed_items INTEGER NOT NULL DEFAULT 0,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, completed_at TIMESTAMP);
        CREATE TABLE job_items (id INTEGER PRIMARY KEY,
            job_id INTEGER NOT NULL REFERENCES download_jobs(id), symbol VARCHAR NOT NULL,
            exchange VARCHAR NOT NULL, timeframe VARCHAR NOT NULL,
            status VARCHAR NOT NULL DEFAULT 'pending', error VARCHAR);
        CREATE TABLE symbol_metadata (symbol VARCHAR, exchange VARCHAR, name VARCHAR NOT NULL,
            sector VARCHAR, industry VARCHAR, market_cap DOUBLE,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (symbol, exchange));
        INSERT INTO watchlist (id, symbol, exchange, name) VALUES (7, 'sbin', 'nse', 'SBI');
        CHECKPOINT;";

    /// A checkpointed desktop-v1 file at `path`.
    fn write_v1(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(DESKTOP_V1).unwrap();
        c.close().unwrap();
    }

    /// Run `f`, then close as a killed process would: nothing checkpointed,
    /// so whatever was committed is left in the WAL.
    fn crash_after(path: &Path, f: impl FnOnce(&Connection)) {
        let c = Connection::open(path).unwrap();
        c.execute_batch("PRAGMA disable_checkpoint_on_shutdown")
            .unwrap();
        f(&c);
        c.close().unwrap();
    }

    fn wal_len(path: &Path) -> u64 {
        std::fs::metadata(wal_path(path))
            .map(|m| m.len())
            .unwrap_or(0)
    }

    fn aside_files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".wal.unreplayable-"))
            .collect()
    }

    #[test]
    fn a_crash_right_after_the_migrations_leaves_an_openable_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("historify.duckdb");
        write_v1(&path);
        crash_after(&path, |c| migrations::run(c).unwrap());
        // The plain DuckDB open, with no recovery, must replay what is left.
        let c = Connection::open(&path).unwrap();
        assert!(migrations::has_column(&c, "watchlist", "display_name").unwrap());
        let sym: String = c
            .query_row("SELECT symbol FROM watchlist WHERE id = 7", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(sym, "SBIN");
        assert!(!migrations::has_column(&c, "job_items", "timeframe").unwrap());
        drop(c);
        assert!(aside_files(dir.path()).is_empty());
    }

    #[test]
    fn an_unreplayable_wal_is_moved_aside_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("historify.duckdb");
        write_v1(&path);
        // The statement DuckDB 1.5.6 writes to the WAL but cannot replay.
        crash_after(&path, |c| c.execute_batch("DROP TABLE job_items").unwrap());
        let wal_bytes = std::fs::read(wal_path(&path)).unwrap();
        let e = match Connection::open(&path) {
            Ok(_) => panic!(
                "DuckDB now replays a dropped foreign-key table; pick another unreplayable WAL"
            ),
            Err(e) => e,
        };
        assert!(is_wal_replay_error(&e), "{}", e);

        let db = HistorifyDb::new(&path).unwrap();
        let rec = db.recovery().cloned().expect("recovery reported");
        assert!(rec.reason.contains(WAL_REPLAY_ERROR));
        assert!(!wal_path(&path).exists() || wal_len(&path) == 0);
        assert_eq!(aside_files(dir.path()), vec![rec.moved_to.clone()]);
        assert_eq!(std::fs::read(&rec.moved_to).unwrap(), wal_bytes);
        // The last checkpointed state was opened and migrated.
        let n: i64 = db
            .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM watchlist", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 1);
        db.close();
        drop(db);
        // A later open is clean and keeps the moved file.
        let again = HistorifyDb::new(&path).unwrap();
        assert!(again.recovery().is_none());
        assert!(rec.moved_to.exists());
    }

    #[test]
    fn other_open_errors_are_not_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("historify.duckdb");
        std::fs::write(&path, b"this is not a database file at all, not even close").unwrap();
        std::fs::write(wal_path(&path), b"wal").unwrap();
        assert!(HistorifyDb::new(&path).is_err());
        assert_eq!(std::fs::read(wal_path(&path)).unwrap(), b"wal");
        assert!(aside_files(dir.path()).is_empty());
    }

    #[test]
    fn a_clean_close_checkpoints_the_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("historify.duckdb");
        let db = HistorifyDb::new(&path).unwrap();
        db.mutate(|c| {
            c.execute(
                "INSERT INTO watchlist (id, symbol, exchange) VALUES (1, 'SBIN', 'NSE')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(wal_len(&path) > 0, "the insert should sit in the WAL");
        // A borrowed connection keeps the instance alive, so only the
        // explicit checkpoint in close() can empty the WAL here.
        let held = db.conn().unwrap();
        db.close();
        assert_eq!(wal_len(&path), 0);
        drop(held);
        let c = Connection::open(&path).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM watchlist", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn wal_path_appends_the_suffix() {
        assert_eq!(
            wal_path(Path::new("/x/historify.duckdb")),
            PathBuf::from("/x/historify.duckdb.wal")
        );
    }
}
