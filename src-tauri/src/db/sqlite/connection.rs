//! SQLite connection utilities

use rusqlite::Connection;
use std::path::Path;

/// Create a new SQLite connection
pub fn create_connection(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open(path)
}
