//! The key vault in `openalgo.db` (KEY-01, maintainer decision 2026-10-13).
//!
//! One row holds the data key (AES-256-GCM, encrypts every stored secret)
//! and the API-key pepper (Argon2 pepper and HMAC key), wrapped with a key
//! derived from the trader's OpenAlgo password with Argon2id, the
//! parameters of the password hash. No operating-system keychain is used
//! on any platform, and nothing outside this database holds key material.
//! A copied database is useless without the password.
//!
//! The row sits beside the password hash, so a password change rewrites
//! both in one transaction: there is no state in which one changed and the
//! other did not (`docs/port/no-keychain.md`).
//!
//! `source` records where the keys came from (a new account, a one-time
//! move from the OS keychain, from `vault.json` or `secrets.dat`, or a fresh
//! start) and `keychain_cleanup` whether the old keychain entries still
//! have to be deleted after such a move. Neither is secret.

use crate::error::{AppError, Result};
use crate::security::crypto::{self, Aad, DataCipher};
use crate::security::{hashing, SecretBytes};
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};

pub const VERSION: i64 = 1;
/// The key-derivation function and its parameters (Argon2id, 19 MiB, two
/// passes, one lane: the OWASP baseline the password hash uses).
pub const KDF: &str = "argon2id-m19456-t2-p1";
const SALT_LEN: usize = 16;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Associated data of the wrapped keys: the same as the retired
/// `vault.json`, so the two formats wrap identically.
fn aad() -> Aad {
    Aad::fixed("vault")
}

/// Migration `082_key_vault`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS key_vault (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            version INTEGER NOT NULL,
            kdf TEXT NOT NULL,
            salt TEXT NOT NULL,
            nonce TEXT NOT NULL,
            ciphertext TEXT NOT NULL,
            source TEXT NOT NULL,
            keychain_cleanup TEXT NOT NULL DEFAULT 'none',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    Ok(())
}

/// Where the keys in the vault came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Made when the account was created.
    Setup,
    /// Moved once from the OS keychain of an earlier build.
    Keychain,
    /// Moved once from `vault.json` (an earlier build's password mode).
    VaultFile,
    /// Moved once from the retired `secrets.dat`.
    LegacyFile,
    /// New keys after a forgotten password or an unreadable keychain: the
    /// encrypted values were dropped.
    Fresh,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Setup => "setup",
            Source::Keychain => "keychain",
            Source::VaultFile => "vault_file",
            Source::LegacyFile => "legacy_file",
            Source::Fresh => "fresh",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "keychain" => Source::Keychain,
            "vault_file" => Source::VaultFile,
            "legacy_file" => Source::LegacyFile,
            "fresh" => Source::Fresh,
            _ => Source::Setup,
        }
    }
}

/// The old keychain entries after a one-time move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeychainCleanup {
    /// The keys never were in a keychain.
    None,
    /// Copied and verified; the entries are still to be deleted (a stop
    /// between the commit and the deletion). Deleted at the next sign-in.
    Pending,
    Done,
    /// The keychain refused the deletion. Never retried: the keychain is
    /// not touched again, and the trader is told how to remove them.
    Failed,
    /// Started fresh because the keychain could not be read: the old
    /// entries may still be there, unused, and are not touched.
    Abandoned,
}

impl KeychainCleanup {
    pub fn as_str(self) -> &'static str {
        match self {
            KeychainCleanup::None => "none",
            KeychainCleanup::Pending => "pending",
            KeychainCleanup::Done => "done",
            KeychainCleanup::Failed => "failed",
            KeychainCleanup::Abandoned => "abandoned",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "pending" => KeychainCleanup::Pending,
            "done" => KeychainCleanup::Done,
            "failed" => KeychainCleanup::Failed,
            "abandoned" => KeychainCleanup::Abandoned,
            _ => KeychainCleanup::None,
        }
    }
}

/// The keys wrapped with a password: what a password change rewrites.
#[derive(Clone, PartialEq, Eq)]
pub struct Wrapped {
    pub salt: String,
    pub nonce: String,
    pub ciphertext: String,
}

impl std::fmt::Debug for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Wrapped([REDACTED])")
    }
}

/// The vault row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRow {
    pub version: i64,
    pub kdf: String,
    pub wrapped: Wrapped,
    pub source: Source,
    pub keychain_cleanup: KeychainCleanup,
}

impl VaultRow {
    pub fn new(wrapped: Wrapped, source: Source, keychain_cleanup: KeychainCleanup) -> Self {
        Self {
            version: VERSION,
            kdf: KDF.into(),
            wrapped,
            source,
            keychain_cleanup,
        }
    }
}

/// Wrap the data key and the pepper with `password`, under a new salt.
pub fn wrap(data_key: &[u8], pepper: &[u8], password: &str) -> Result<Wrapped> {
    if data_key.len() != crypto::KEY_SIZE || pepper.len() != hashing::PEPPER_SIZE {
        return Err(AppError::Encryption(
            "key material has the wrong size".into(),
        ));
    }
    let salt: [u8; SALT_LEN] = rand::Rng::gen(&mut rand::rngs::OsRng);
    let kek = zeroize::Zeroizing::new(hashing::derive_kek(password, &salt)?);
    let cipher = DataCipher::new(&kek[..])?;
    let mut blob = zeroize::Zeroizing::new(Vec::with_capacity(data_key.len() + pepper.len()));
    blob.extend_from_slice(data_key);
    blob.extend_from_slice(pepper);
    let (ciphertext, nonce) = cipher.encrypt(&blob[..], &aad())?;
    Ok(Wrapped {
        salt: B64.encode(salt),
        nonce,
        ciphertext,
    })
}

/// Fewest salt bytes Argon2 takes.
const MIN_SALT_LEN: usize = 8;
/// The sealed data key and pepper: 64 bytes and the 16-byte GCM tag.
const SEALED_LEN: usize = crypto::KEY_SIZE + hashing::PEPPER_SIZE + 16;

/// Whether `row` is a vault this build can open, checked without any
/// password: version, KDF, and the encoding and size of the salt, nonce and
/// sealed keys. A sign-in checks it before it looks at the name or the
/// password, so a damaged vault answers the same whatever was typed and is
/// never read as a wrong password (KEY-01 security review).
pub fn check(row: &VaultRow) -> Result<()> {
    let damaged = || AppError::Encryption("the key vault is damaged".into());
    if row.version != VERSION || row.kdf != KDF {
        return Err(damaged());
    }
    let len = |b64: &str| B64.decode(b64).map(|v| v.len()).map_err(|_| damaged());
    if len(&row.wrapped.salt)? < MIN_SALT_LEN
        || len(&row.wrapped.nonce)? != crypto::NONCE_SIZE
        || len(&row.wrapped.ciphertext)? != SEALED_LEN
    {
        return Err(damaged());
    }
    Ok(())
}

/// The data key and the pepper, or `None` when `password` does not open
/// the vault. An unreadable row is an error, never `None`: a damaged vault
/// must not read as a wrong password.
pub fn unwrap(row: &VaultRow, password: &str) -> Result<Option<(SecretBytes, SecretBytes)>> {
    let damaged = || AppError::Encryption("the key vault is damaged".into());
    check(row)?;
    let salt = B64.decode(&row.wrapped.salt).map_err(|_| damaged())?;
    let kek = zeroize::Zeroizing::new(hashing::derive_kek(password, &salt)?);
    let cipher = DataCipher::new(&kek[..])?;
    let plain = match cipher.decrypt_bytes(&row.wrapped.ciphertext, &row.wrapped.nonce, &aad()) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let bytes = plain.expose();
    if bytes.len() != crypto::KEY_SIZE + hashing::PEPPER_SIZE {
        return Err(damaged());
    }
    Ok(Some((
        SecretBytes::new(bytes[..crypto::KEY_SIZE].to_vec()),
        SecretBytes::new(bytes[crypto::KEY_SIZE..].to_vec()),
    )))
}

pub fn read(conn: &Connection) -> Result<Option<VaultRow>> {
    Ok(conn
        .query_row(
            "SELECT version, kdf, salt, nonce, ciphertext, source, keychain_cleanup
             FROM key_vault WHERE id = 1",
            [],
            |r| {
                Ok(VaultRow {
                    version: r.get(0)?,
                    kdf: r.get(1)?,
                    wrapped: Wrapped {
                        salt: r.get(2)?,
                        nonce: r.get(3)?,
                        ciphertext: r.get(4)?,
                    },
                    source: Source::parse(&r.get::<_, String>(5)?),
                    keychain_cleanup: KeychainCleanup::parse(&r.get::<_, String>(6)?),
                })
            },
        )
        .optional()?)
}

pub fn exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM key_vault WHERE id = 1)",
        [],
        |r| r.get(0),
    )?)
}

/// Store `row` as the vault, replacing any. Run inside the caller's
/// transaction.
pub fn put(conn: &Connection, row: &VaultRow) -> Result<()> {
    conn.execute(
        "INSERT INTO key_vault (id, version, kdf, salt, nonce, ciphertext, source, keychain_cleanup)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET version = excluded.version, kdf = excluded.kdf,
             salt = excluded.salt, nonce = excluded.nonce, ciphertext = excluded.ciphertext,
             source = excluded.source, keychain_cleanup = excluded.keychain_cleanup,
             created_at = datetime('now'), updated_at = datetime('now')",
        params![
            row.version,
            row.kdf,
            row.wrapped.salt,
            row.wrapped.nonce,
            row.wrapped.ciphertext,
            row.source.as_str(),
            row.keychain_cleanup.as_str(),
        ],
    )?;
    Ok(())
}

/// Replace the wrapping (a password change), keeping where the keys came
/// from. Fails when there is no vault. Run inside the caller's transaction,
/// beside the password hash.
pub fn rewrap(conn: &Connection, wrapped: &Wrapped) -> Result<()> {
    let n = conn.execute(
        "UPDATE key_vault SET version = ?1, kdf = ?2, salt = ?3, nonce = ?4, ciphertext = ?5,
             updated_at = datetime('now')
         WHERE id = 1",
        params![
            VERSION,
            KDF,
            wrapped.salt,
            wrapped.nonce,
            wrapped.ciphertext
        ],
    )?;
    if n != 1 {
        return Err(AppError::Encryption(
            "there is no key vault to update".into(),
        ));
    }
    Ok(())
}

pub fn set_keychain_cleanup(conn: &Connection, state: KeychainCleanup) -> Result<()> {
    conn.execute(
        "UPDATE key_vault SET keychain_cleanup = ?1, updated_at = datetime('now') WHERE id = 1",
        [state.as_str()],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM key_vault", [])?;
    Ok(())
}

/// One stored ciphertext, to prove a key is the one the data was written
/// with ("decrypt a known row").
#[derive(Debug, Clone)]
pub struct Witness {
    pub ciphertext: String,
    pub nonce: String,
    pub aad: Aad,
}

/// The first encrypted value in the database: the account's TOTP secret,
/// else the API key, else saved broker keys, else a stored broker session,
/// else the tunnel's ngrok authtoken, else an Agent key.
/// `None` when nothing is encrypted yet (then any key will do).
pub fn witness(conn: &Connection) -> Result<Option<Witness>> {
    let first = |sql: &str| -> Result<Option<(String, String, String)>> {
        Ok(conn
            .query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?)
    };
    if let Some((user, ct, n)) = first(
        "SELECT username, totp_secret_encrypted, totp_nonce FROM users
         WHERE totp_secret_encrypted IS NOT NULL AND totp_secret_encrypted <> ''
           AND totp_nonce IS NOT NULL ORDER BY id LIMIT 1",
    )? {
        return Ok(Some(Witness {
            ciphertext: ct,
            nonce: n,
            aad: Aad::new("users", "totp_secret", &user),
        }));
    }
    if let Some((name, ct, n)) = first(
        "SELECT name, encrypted_key, nonce FROM api_keys
         WHERE encrypted_key <> '' ORDER BY id LIMIT 1",
    )? {
        return Ok(Some(Witness {
            ciphertext: ct,
            nonce: n,
            aad: super::api_keys::aad(&name),
        }));
    }
    if let Some((broker, ct, n)) = first(
        "SELECT broker_id, api_key_encrypted, api_key_nonce FROM broker_credentials
         WHERE api_key_encrypted IS NOT NULL AND api_key_encrypted <> '' ORDER BY broker_id LIMIT 1",
    )? {
        return Ok(Some(Witness {
            ciphertext: ct,
            nonce: n,
            aad: Aad::new("broker_credentials", "api_key", &broker),
        }));
    }
    if let Some((broker, ct, n)) = first(
        "SELECT broker_id, auth_token_encrypted, auth_token_nonce FROM auth
         WHERE auth_token_encrypted <> '' AND auth_token_nonce IS NOT NULL ORDER BY id LIMIT 1",
    )? {
        return Ok(Some(Witness {
            ciphertext: ct,
            nonce: n,
            aad: Aad::new("auth", "auth_token", &broker),
        }));
    }
    if let Some(w) = super::tunnel::witness(conn)? {
        return Ok(Some(w));
    }
    crate::agent::models::secrets::witness(conn)
}

/// Every value the data key encrypts, dropped for a fresh start (a
/// forgotten password, or keys that cannot be read). Run inside the
/// caller's transaction, which then stores new keys, a new password hash, a
/// new API key (keeping the order mode) and a new TOTP secret.
///
/// Kept: the account (name, email), settings, strategies, Chartink and
/// webhook addresses, AI client (MCP) tokens, watchlists, logs, history and
/// sandbox mode, none of which is encrypted. Dropped: saved broker keys (the
/// broker id, client id and redirect URL stay, so only the keys are typed
/// again), stored broker sessions, pending broker sign-ins, the TOTP secret
/// with every "require the authenticator" switch (TOTP is set up again),
/// the Telegram bot token and linked users' saved API keys, the WhatsApp
/// pairing, the tunnel's ngrok authtoken (its switch and domain stay), and
/// the Agent's keys and ChatGPT sign-in (its models stay, untested where
/// they needed one: `agent::models::secrets::wipe`).
///
/// A new encrypted column must be added here and to `witness` (and the
/// test that fills every one of them).
pub fn drop_encrypted_values(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "UPDATE broker_credentials SET api_key_encrypted = '', api_key_nonce = '',
             api_secret_encrypted = NULL, api_secret_nonce = NULL,
             api_key_market_encrypted = NULL, api_key_market_nonce = NULL,
             api_secret_market_encrypted = NULL, api_secret_market_nonce = NULL,
             updated_at = datetime('now');
         DELETE FROM auth;
         DELETE FROM pending_oauth;
         UPDATE users SET totp_secret_encrypted = NULL, totp_nonce = NULL, totp_enabled = 0,
             totp_required_for_login = 0, totp_required_for_password_reset = 0,
             totp_required_for_mcp = 0, updated_at = datetime('now');
         UPDATE bot_config SET token = NULL, is_active = 0;
         UPDATE telegram_users SET encrypted_api_key = NULL;
         UPDATE whatsapp_users SET encrypted_api_key = NULL;",
    )?;
    crate::messaging::whatsapp::db::clear_session(conn)?;
    super::tunnel::forget_authtoken(conn)?;
    crate::agent::models::secrets::wipe(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, super::super::SqliteDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = super::super::SqliteDb::new(&dir.path().join("openalgo.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn wrap_and_unwrap_round_trip() {
        let w = wrap(&[1u8; 32], &[2u8; 32], "Pw@12345").unwrap();
        let row = VaultRow::new(w.clone(), Source::Setup, KeychainCleanup::None);
        let (k, p) = unwrap(&row, "Pw@12345").unwrap().unwrap();
        assert_eq!(k.expose(), &[1u8; 32]);
        assert_eq!(p.expose(), &[2u8; 32]);
        assert!(
            unwrap(&row, "Wrong@123").unwrap().is_none(),
            "wrong password"
        );
        // A fresh salt each time: the same keys never wrap the same way.
        assert_ne!(wrap(&[1u8; 32], &[2u8; 32], "Pw@12345").unwrap(), w);
        // A damaged row is an error, not a wrong password.
        let mut bad = row.clone();
        bad.kdf = "pbkdf2".into();
        assert!(unwrap(&bad, "Pw@12345").is_err());
        let mut bad = row;
        bad.wrapped.salt = "%%%".into();
        assert!(unwrap(&bad, "Pw@12345").is_err());
        assert!(wrap(&[1u8; 16], &[2u8; 32], "Pw@12345").is_err());
    }

    /// KEY-01: a fresh start drops every value the data key encrypts and
    /// keeps the account, its settings and everything that is not
    /// encrypted.
    #[test]
    fn a_fresh_start_drops_only_the_encrypted_values() {
        let (_d, db) = db();
        let conn = db.conn().unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, email, password_hash, totp_secret_encrypted, totp_nonce,
                 totp_enabled, totp_required_for_login, totp_required_for_password_reset,
                 totp_required_for_mcp)
             VALUES ('trader', 't@example.com', 'h', 'ct', 'n', 1, 1, 1, 1);
             INSERT INTO broker_credentials (broker_id, api_key_encrypted, api_key_nonce,
                 api_secret_encrypted, api_secret_nonce, api_key_market_encrypted,
                 api_key_market_nonce, api_secret_market_encrypted, api_secret_market_nonce,
                 client_id, redirect_url)
             VALUES ('zerodha', 'k', 'kn', 's', 'sn', 'km', 'kmn', 'sm', 'smn', 'AB1234',
                 'http://127.0.0.1:5000/zerodha/callback');
             INSERT INTO auth (broker_id, auth_token_encrypted, auth_token_nonce)
             VALUES ('zerodha', 'tok', 'n');
             INSERT INTO pending_oauth (state_hash, broker, created_at, expires_at)
             VALUES ('s', 'zerodha', '2026-10-13', '2026-10-13');
             UPDATE bot_config SET token = 'v1:n:c', is_active = 1;
             INSERT INTO telegram_users (telegram_id, openalgo_username, encrypted_api_key)
             VALUES (7, 'trader', 'v1:n:c');
             INSERT INTO whatsapp_users (whatsapp_jid, phone_number, openalgo_username,
                 encrypted_api_key)
             VALUES ('j', '91', 'trader', 'v1:n:c');
             INSERT OR REPLACE INTO whatsapp_config (id, session_blob, is_paired, is_active)
             VALUES (1, 'v1:n:c', 1, 1);
             UPDATE settings SET ngrok_authtoken = 'v1:n:c', ngrok_allow = 1,
                 ngrok_domain = 'desk.ngrok.app' WHERE id = 1;
             INSERT INTO ag_secret (name, ciphertext, fingerprint)
             VALUES ('provider:openai', 'v1:n:c', '...abcd sha256:0123456789ab');
             INSERT INTO ag_provider_model (provider_kind, model_name, display_name,
                 is_default, last_test_ok, created_at, updated_at)
             VALUES ('openai', 'gpt-4o', 'GPT-4o', 1, 1, 't', 't');
             INSERT INTO ag_setting (key, value) VALUES ('trading_enabled', 'true');",
        )
        .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        drop_encrypted_values(&conn).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        let one = |sql: &str| -> Option<String> { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        let n = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        // Dropped.
        assert_eq!(one("SELECT totp_secret_encrypted FROM users"), None);
        assert_eq!(
            n(
                "SELECT totp_enabled + totp_required_for_login + totp_required_for_password_reset
                 + totp_required_for_mcp FROM users"
            ),
            0,
            "TOTP is set up again"
        );
        assert_eq!(
            one("SELECT api_key_encrypted FROM broker_credentials").as_deref(),
            Some("")
        );
        for col in [
            "api_secret_encrypted",
            "api_key_market_encrypted",
            "api_secret_market_encrypted",
        ] {
            assert_eq!(
                one(&format!("SELECT {} FROM broker_credentials", col)),
                None,
                "{}",
                col
            );
        }
        assert_eq!(n("SELECT COUNT(*) FROM auth"), 0);
        assert_eq!(n("SELECT COUNT(*) FROM pending_oauth"), 0);
        assert_eq!(one("SELECT token FROM bot_config"), None);
        assert_eq!(one("SELECT encrypted_api_key FROM telegram_users"), None);
        assert_eq!(one("SELECT encrypted_api_key FROM whatsapp_users"), None);
        assert_eq!(one("SELECT session_blob FROM whatsapp_config"), None);
        assert_eq!(one("SELECT ngrok_authtoken FROM settings"), None);
        assert_eq!(n("SELECT COUNT(*) FROM ag_secret"), 0);
        assert_eq!(
            n("SELECT COUNT(*) FROM ag_provider_model WHERE is_default = 1 OR last_test_ok = 1"),
            0,
            "a model whose key went is tested again before it runs"
        );
        assert!(
            witness(&conn).unwrap().is_none(),
            "nothing encrypted is left"
        );
        // Kept.
        assert_eq!(one("SELECT username FROM users").as_deref(), Some("trader"));
        assert_eq!(
            one("SELECT email FROM users").as_deref(),
            Some("t@example.com")
        );
        assert_eq!(
            one("SELECT client_id FROM broker_credentials").as_deref(),
            Some("AB1234")
        );
        assert!(one("SELECT redirect_url FROM broker_credentials").is_some());
        assert_eq!(n("SELECT COUNT(*) FROM telegram_users"), 1);
        assert_eq!(n("SELECT COUNT(*) FROM whatsapp_users"), 1);
        assert_eq!(n("SELECT ngrok_allow FROM settings"), 1);
        assert_eq!(
            one("SELECT ngrok_domain FROM settings").as_deref(),
            Some("desk.ngrok.app")
        );
        assert!(super::super::credentials::list_configured(&conn)
            .unwrap()
            .is_empty());
        assert_eq!(n("SELECT COUNT(*) FROM ag_provider_model"), 1);
        assert_eq!(
            one("SELECT value FROM ag_setting WHERE key = 'trading_enabled'").as_deref(),
            Some("true")
        );
    }

    #[test]
    fn the_witness_is_the_first_encrypted_value() {
        let (_d, db) = db();
        let conn = db.conn().unwrap();
        assert!(witness(&conn).unwrap().is_none());
        // An Agent key, when it is the only encrypted value.
        conn.execute_batch(
            "INSERT INTO ag_secret (name, ciphertext) VALUES ('provider:openai', 'v1:an:ac')",
        )
        .unwrap();
        let w = witness(&conn).unwrap().unwrap();
        assert_eq!((w.ciphertext.as_str(), w.nonce.as_str()), ("ac", "an"));
        assert_eq!(
            w.aad,
            Aad::new("ag_secret", "ciphertext", "provider:openai")
        );
        // The tunnel's authtoken comes before it.
        conn.execute_batch("UPDATE settings SET ngrok_authtoken = 'v1:nt:ct' WHERE id = 1")
            .unwrap();
        let w = witness(&conn).unwrap().unwrap();
        assert_eq!((w.ciphertext.as_str(), w.nonce.as_str()), ("ct", "nt"));
        assert_eq!(w.aad, super::super::tunnel::authtoken_aad());
        conn.execute_batch(
            "INSERT INTO auth (broker_id, auth_token_encrypted, auth_token_nonce)
             VALUES ('fyers', 'tok', 'tn');",
        )
        .unwrap();
        let w = witness(&conn).unwrap().unwrap();
        assert_eq!((w.ciphertext.as_str(), w.nonce.as_str()), ("tok", "tn"));
        assert_eq!(w.aad, Aad::new("auth", "auth_token", "fyers"));
        conn.execute_batch(
            "INSERT INTO users (username, password_hash, totp_secret_encrypted, totp_nonce)
             VALUES ('trader', 'h', 'ct', 'n');",
        )
        .unwrap();
        let w = witness(&conn).unwrap().unwrap();
        assert_eq!(w.aad, Aad::new("users", "totp_secret", "trader"));
    }

    /// The vault is in openalgo.db: one row, replaced and rewrapped there,
    /// and its debug form never shows the wrapped bytes.
    #[test]
    fn one_row_in_the_main_database() {
        let (_d, db) = db();
        let conn = db.conn().unwrap();
        assert!(read(&conn).unwrap().is_none());
        assert!(rewrap(&conn, &wrap(&[1u8; 32], &[2u8; 32], "a").unwrap()).is_err());
        let row = VaultRow::new(
            wrap(&[1u8; 32], &[2u8; 32], "Pw@12345").unwrap(),
            Source::Keychain,
            KeychainCleanup::Pending,
        );
        put(&conn, &row).unwrap();
        assert_eq!(read(&conn).unwrap().unwrap(), row);
        put(&conn, &row).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM key_vault", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let w2 = wrap(&[1u8; 32], &[2u8; 32], "Next@1234").unwrap();
        rewrap(&conn, &w2).unwrap();
        set_keychain_cleanup(&conn, KeychainCleanup::Done).unwrap();
        let back = read(&conn).unwrap().unwrap();
        assert_eq!(back.source, Source::Keychain, "the source is kept");
        assert_eq!(back.keychain_cleanup, KeychainCleanup::Done);
        assert!(unwrap(&back, "Pw@12345").unwrap().is_none());
        assert!(unwrap(&back, "Next@1234").unwrap().is_some());
        assert!(!format!("{:?}", back).contains(&back.wrapped.ciphertext));
        delete(&conn).unwrap();
        assert!(!exists(&conn).unwrap());
    }
}
