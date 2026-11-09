//! The r2d2 connection manager of the SQLite stores.
//!
//! Until 1.0.7 this was r2d2_sqlite's `SqliteConnectionManager`. Its newest
//! release (0.35.0) needs rusqlite 0.40, whose bundled SQLite 3.53.2 has
//! RUSTSEC-2026-0459 (CVE-2026-119375: page-cache memory read back through
//! a `-wal` of another page size); the fix is SQLite 3.54 in rusqlite 0.41,
//! and both cannot be linked into one build (one `sqlite3` library per
//! build). This is the part of r2d2_sqlite 0.25 the stores used, unchanged:
//! a file opened with rusqlite's default flags, `init` run on every new
//! connection, an empty batch as the check before a checkout, and a
//! connection never judged broken. Once an r2d2_sqlite release supports the
//! rusqlite in use, it can take this module's place again.

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// The set-up every new connection runs before the pool hands it out.
type Init = dyn Fn(&mut Connection) -> rusqlite::Result<()> + Send + Sync + 'static;

/// Opens the connections of one database file for an r2d2 pool.
pub struct SqliteConnectionManager {
    path: PathBuf,
    init: Option<Box<Init>>,
}

impl SqliteConnectionManager {
    /// Connections to the database at `path`, created if it is missing.
    pub(crate) fn file(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            init: None,
        }
    }

    /// Run `init` on every connection this manager opens, before the pool
    /// hands it out (the store's pragmas and busy handler).
    pub(crate) fn with_init<F>(self, init: F) -> Self
    where
        F: Fn(&mut Connection) -> rusqlite::Result<()> + Send + Sync + 'static,
    {
        Self {
            init: Some(Box::new(init)),
            ..self
        }
    }
}

impl r2d2::ManageConnection for SqliteConnectionManager {
    type Connection = Connection;
    type Error = rusqlite::Error;

    fn connect(&self) -> rusqlite::Result<Connection> {
        let mut conn = Connection::open_with_flags(&self.path, OpenFlags::default())?;
        if let Some(init) = &self.init {
            init(&mut conn)?;
        }
        Ok(conn)
    }

    /// An empty batch, as r2d2_sqlite 0.25 ran: it prepares nothing and
    /// touches no file, so a checkout costs no statement (0.34 and later
    /// run `SELECT 1` by default).
    fn is_valid(&self, conn: &mut Connection) -> rusqlite::Result<()> {
        conn.execute_batch("")
    }

    fn has_broken(&self, _: &mut Connection) -> bool {
        false
    }
}

#[cfg(test)]
mod tests;
