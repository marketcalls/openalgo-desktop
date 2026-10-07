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

pub mod migrations;

use crate::error::{AppError, Result};
use duckdb::{Config, Connection};
use parking_lot::Mutex;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Trader-facing text when the store cannot be used.
pub const STORE_UNAVAILABLE: &str =
    "The Historify database is not available. Restart OpenAlgo and try again.";

struct Inner {
    root: Mutex<Option<Connection>>,
    write_lock: Mutex<()>,
    live: AtomicUsize,
    path: PathBuf,
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
    pub fn new(path: &Path) -> Result<Self> {
        let root = Connection::open_with_flags(path, config()?)?;
        let db = Self::from_root(root, path.to_path_buf());
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
        let db = Self::from_root(root, PathBuf::new());
        {
            let c = db.conn()?;
            migrations::run(&c)?;
        }
        Ok(db)
    }

    fn from_root(root: Connection, path: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                root: Mutex::new(Some(root)),
                write_lock: Mutex::new(()),
                live: AtomicUsize::new(0),
                path,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Borrow a connection. Fails once the store is closed.
    pub fn conn(&self) -> Result<DuckConn> {
        let root = self.inner.root.lock();
        let Some(r) = root.as_ref() else {
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

    pub fn is_open(&self) -> bool {
        self.inner.root.lock().is_some()
    }

    /// Close the root connection. The file is released once every borrowed
    /// connection has been dropped; later borrows fail.
    pub fn close(&self) {
        let root = self.inner.root.lock().take();
        if let Some(r) = root {
            if let Err((_, e)) = r.close() {
                tracing::warn!("Closing the Historify database reported: {}", e);
            }
        }
    }
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
}
