//! User management

use crate::db::sqlite::models::User;
use crate::error::Result;
use crate::security::SecurityManager;
use rusqlite::Connection;

/// Verify user credentials
pub fn verify_user(
    conn: &Connection,
    username: &str,
    password: &str,
    security: &SecurityManager,
) -> Result<Option<User>> {
    let result = conn.query_row(
        "SELECT id, username, password_hash, created_at FROM users WHERE username = ?",
        [username],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    );

    match result {
        Ok((id, username, password_hash, created_at)) => {
            if security.verify_password(password, &password_hash)? {
                Ok(Some(User {
                    id,
                    username,
                    created_at,
                }))
            } else {
                Ok(None)
            }
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Create a new user
pub fn create_user(
    conn: &Connection,
    username: &str,
    password: &str,
    security: &SecurityManager,
) -> Result<User> {
    let password_hash = security.hash_password(password)?;

    conn.execute(
        "INSERT INTO users (username, password_hash) VALUES (?, ?)",
        rusqlite::params![username, password_hash],
    )?;

    let id = conn.last_insert_rowid();

    Ok(User {
        id,
        username: username.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// Check if any user exists
pub fn has_user(conn: &Connection) -> Result<bool> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
    Ok(count > 0)
}

/// Delete all users (for password reset when pepper changes)
pub fn delete_all_users(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM users", [])?;
    Ok(())
}
