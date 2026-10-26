//! The built-in tunnel's settings, in the one `settings` row. The switch is
//! the web's `ngrok_allow` (`NGROK_ALLOW`, which starts ngrok on the web
//! too); the authtoken and the reserved domain are desktop columns, since
//! the web reads the token from ngrok's own configuration file and the
//! domain from `HOST_SERVER`.
//!
//! The authtoken is stored sealed with the data key
//! (`messaging::sealed`, `v1:<nonce>:<ciphertext>`), bound by associated
//! data to this row and column ([`authtoken_aad`]), and never read back
//! out of Rust.

use crate::error::Result;
use crate::security::crypto::Aad;
use rusqlite::{params, Connection, OptionalExtension};

/// The associated data binding the sealed authtoken to its row and column.
pub fn authtoken_aad() -> Aad {
    Aad::new("settings", "ngrok_authtoken", "1")
}

/// The stored tunnel settings. `authtoken` is the sealed value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredTunnel {
    pub enabled: bool,
    pub authtoken: Option<String>,
    pub domain: Option<String>,
}

/// Read the settings row's tunnel columns. Empty values read as unset.
pub fn read(conn: &Connection) -> Result<StoredTunnel> {
    Ok(conn.query_row(
        "SELECT ngrok_allow, ngrok_authtoken, ngrok_domain FROM settings WHERE id = 1",
        [],
        |r| {
            Ok(StoredTunnel {
                enabled: r.get::<_, i64>(0)? != 0,
                authtoken: r.get::<_, Option<String>>(1)?.filter(|s| !s.is_empty()),
                domain: r.get::<_, Option<String>>(2)?.filter(|s| !s.is_empty()),
            })
        },
    )?)
}

/// What one save changes. `None` keeps a field; `Some(None)` clears it.
#[derive(Debug, Default)]
pub struct TunnelUpdate {
    pub enabled: Option<bool>,
    /// The sealed authtoken.
    pub authtoken: Option<Option<String>>,
    pub domain: Option<Option<String>>,
}

/// Apply `u` to the settings row, in one statement.
pub fn save(conn: &Connection, u: &TunnelUpdate) -> Result<()> {
    conn.execute(
        "UPDATE settings SET
            ngrok_allow = CASE WHEN ?1 IS NULL THEN ngrok_allow ELSE ?1 END,
            ngrok_authtoken = CASE WHEN ?2 THEN ?3 ELSE ngrok_authtoken END,
            ngrok_domain = CASE WHEN ?4 THEN ?5 ELSE ngrok_domain END,
            updated_at = datetime('now')
         WHERE id = 1",
        params![
            u.enabled,
            u.authtoken.is_some(),
            u.authtoken.clone().flatten(),
            u.domain.is_some(),
            u.domain.clone().flatten(),
        ],
    )?;
    Ok(())
}

/// Drop the saved authtoken: a fresh start or an account reset, after
/// which the key it was sealed with is gone. The switch and the domain
/// stay; the tunnel waits for a new authtoken.
pub fn forget_authtoken(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE settings SET ngrok_authtoken = NULL WHERE id = 1",
        [],
    )?;
    Ok(())
}

/// The sealed authtoken as proof of the data key
/// (`key_vault::witness`).
pub fn witness(conn: &Connection) -> Result<Option<super::key_vault::Witness>> {
    let Some(stored) = read(conn)?.authtoken else {
        return Ok(None);
    };
    Ok(
        crate::messaging::sealed::parts(&stored).map(|(nonce, ct)| super::key_vault::Witness {
            ciphertext: ct.to_string(),
            nonce: nonce.to_string(),
            aad: authtoken_aad(),
        }),
    )
}

/// The domains ngrok itself hands out. A Host Server URL on one of them
/// was the trader's ngrok address before the tunnel was built in.
const NGROK_SUFFIXES: [&str; 6] = [
    ".ngrok.app",
    ".ngrok-free.app",
    ".ngrok.dev",
    ".ngrok-free.dev",
    ".ngrok.io",
    ".ngrok.pizza",
];

/// The ngrok domain a stored Host Server URL names, if it names one.
fn ngrok_domain_of(host_server: &str) -> Option<String> {
    let host = url::Url::parse(host_server.trim())
        .ok()?
        .host_str()?
        .to_ascii_lowercase();
    NGROK_SUFFIXES
        .iter()
        .any(|s| host.ends_with(s))
        .then_some(host)
}

/// Migration 096: the authtoken and the reserved domain. The domain is
/// filled from the Host Server URL when that names an ngrok domain (the
/// address an earlier build's trader ran ngrok on, as the web derives it
/// from `HOST_SERVER`); a domain already stored is never replaced.
pub fn migrate(conn: &Connection) -> Result<()> {
    super::migrations::add_column(conn, "settings", "ngrok_authtoken", "TEXT")?;
    super::migrations::add_column(conn, "settings", "ngrok_domain", "TEXT")?;
    let host_server: Option<String> = conn
        .query_row("SELECT host_server FROM settings WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    if let Some(domain) = host_server.as_deref().and_then(ngrok_domain_of) {
        conn.execute(
            "UPDATE settings SET ngrok_domain = ?1
             WHERE id = 1 AND (ngrok_domain IS NULL OR ngrok_domain = '')",
            params![domain],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
