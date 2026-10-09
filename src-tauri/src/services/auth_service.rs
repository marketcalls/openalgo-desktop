//! OpenAlgo account: setup, sign-in, password change and reset, account reset.
//! Mirrors `blueprints/core.py` and `blueprints/auth.py` of the web.

use crate::db::sqlite::{api_keys, auth, credentials, user};
use crate::error::{AppError, Result};
use crate::security::{totp, KeyMode, Secret};
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;

/// The answer to a wrong current password on a password change.
pub const WRONG_CURRENT_PASSWORD: &str = "Current password is incorrect";

/// Pepper of the stand-in hash checked for a name that is not the
/// account's (see [`AuthService::verify_credentials`]). Not a secret.
const STAND_IN_PEPPER: [u8; 32] = [0x5a; 32];

/// A fixed Argon2id hash with the same parameters as the account's, made
/// once per process, that every password for a name that is not the
/// account's is checked against: the same work as a wrong password.
fn stand_in_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        crate::security::hashing::hash_password(&STAND_IN_PEPPER, "openalgo-no-such-user")
            .unwrap_or_default()
    })
}

/// How many sign-ins were checked against the stand-in hash (tests read
/// it to see the unknown-name path ran the same work).
pub static STAND_IN_CHECKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Same rules and messages as the web's `validate_password_strength`.
pub fn validate_password_strength(password: &str) -> std::result::Result<(), &'static str> {
    if password.is_empty() {
        return Err("Password is required");
    }
    if password.chars().count() < 8 {
        return Err("Password must be at least 8 characters long");
    }
    if !password.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("Password must contain at least 1 uppercase letter (A-Z)");
    }
    if !password.chars().any(|c| c.is_ascii_lowercase()) {
        return Err("Password must contain at least 1 lowercase letter (a-z)");
    }
    if !password.chars().any(|c| c.is_ascii_digit()) {
        return Err("Password must contain at least 1 number (0-9)");
    }
    if !password.chars().any(|c| "!@#$%^&*".contains(c)) {
        return Err("Password must contain at least 1 special character (!@#$%^&*)");
    }
    Ok(())
}

fn valid_email(email: &str) -> bool {
    let mut parts = email.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        }
        _ => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoginOutcome {
    Success(String),
    TotpRequired(String),
    Invalid,
}

pub struct AuthService;

impl AuthService {
    pub fn needs_setup(state: &AppState) -> Result<bool> {
        let conn = state.sqlite.conn()?;
        Ok(!user::has_user(&conn)?)
    }

    /// Create the one account, its TOTP secret and its API key.
    pub fn setup(state: &AppState, username: &str, email: &str, password: &str) -> Result<()> {
        if !Self::needs_setup(state)? {
            return Err(AppError::Validation(
                "An account already exists on this computer. Sign in instead.".into(),
            ));
        }
        let username = username.trim();
        if username.is_empty() || username.len() > 80 {
            return Err(AppError::Validation("Enter a username.".into()));
        }
        if !valid_email(email.trim()) {
            return Err(AppError::Validation("Enter a valid email address.".into()));
        }
        validate_password_strength(password).map_err(|m| AppError::Validation(m.into()))?;
        // Password mode: the first password creates the key vault.
        if !state.security.unlock_with_password(password)? {
            return Err(AppError::Auth("Could not open secure storage.".into()));
        }
        let hash = state.security.hash_password(password)?;
        let secret = totp::generate_secret();
        {
            let conn = state.sqlite.conn()?;
            user::insert(
                &conn,
                &state.security,
                username,
                email.trim(),
                &hash,
                &secret,
            )?;
        }
        ApiKeyService::regenerate(state, username)?;
        crate::db::sqlite::data_migrations::run(&state.sqlite, &state.security)?;
        tracing::info!("Account created");
        Ok(())
    }

    /// Check a username and password. Does not create a web session.
    pub fn verify_credentials(
        state: &AppState,
        username: &str,
        password: &str,
    ) -> Result<LoginOutcome> {
        let row = {
            let conn = state.sqlite.conn()?;
            user::find_by_username(&conn, username)?
        };
        let Some(row) = row else {
            // A name that is not the account's is checked against a fixed
            // stand-in hash with the account's Argon2 parameters: the same
            // work, the same answer and (in the caller) the same budget as
            // a wrong password, so nothing tells which names exist.
            STAND_IN_CHECKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = crate::security::hashing::verify_password(
                &STAND_IN_PEPPER,
                password,
                stand_in_hash(),
            );
            return Ok(LoginOutcome::Invalid);
        };
        if state.security.mode() == KeyMode::Password
            && !state.security.unlock_with_password(password)?
        {
            return Ok(LoginOutcome::Invalid);
        }
        if !state
            .security
            .verify_password(password, &row.password_hash)?
        {
            return Ok(LoginOutcome::Invalid);
        }
        // Now unlocked in password mode: finish any pending re-encryption.
        crate::db::sqlite::data_migrations::run(&state.sqlite, &state.security)?;
        Self::ensure_totp_secret(state, &row)?;
        if row.is_totp_required_for("login") {
            Ok(LoginOutcome::TotpRequired(row.username))
        } else {
            Ok(LoginOutcome::Success(row.username))
        }
    }

    /// Accounts created by earlier desktop builds have no TOTP secret.
    fn ensure_totp_secret(state: &AppState, row: &user::UserRow) -> Result<()> {
        if row.totp_secret(&state.security)?.is_none() {
            let conn = state.sqlite.conn()?;
            user::set_totp_secret(
                &conn,
                &state.security,
                &row.username,
                &totp::generate_secret(),
            )?;
        }
        Ok(())
    }

    pub fn verify_totp_for(state: &AppState, username: &str, code: &str) -> Result<bool> {
        let row = {
            let conn = state.sqlite.conn()?;
            user::find_by_username(&conn, username)?
        };
        let Some(row) = row else { return Ok(false) };
        let Some(secret) = row.totp_secret(&state.security)? else {
            return Ok(false);
        };
        let now = state.now().timestamp().max(0) as u64;
        Ok(totp::verify(secret.expose(), code, now))
    }

    pub fn verify_totp_by_email(state: &AppState, email: &str, code: &str) -> Result<bool> {
        let row = {
            let conn = state.sqlite.conn()?;
            user::find_by_email(&conn, email)?
        };
        match row {
            Some(r) => Self::verify_totp_for(state, &r.username, code),
            None => Ok(false),
        }
    }

    pub fn change_password(
        state: &AppState,
        username: &str,
        old: &str,
        new: &str,
        confirm: &str,
    ) -> Result<()> {
        let row = {
            let conn = state.sqlite.conn()?;
            user::find_by_username(&conn, username)?
        }
        .ok_or_else(|| AppError::Validation(WRONG_CURRENT_PASSWORD.into()))?;
        if !state.security.verify_password(old, &row.password_hash)? {
            return Err(AppError::Validation(WRONG_CURRENT_PASSWORD.into()));
        }
        if new != confirm {
            return Err(AppError::Validation("New passwords do not match".into()));
        }
        validate_password_strength(new).map_err(|m| AppError::Validation(m.into()))?;
        Self::set_password(state, row.id, new)
    }

    /// Reset by email + TOTP (the web's TOTP path; no email step on desktop).
    pub fn reset_password(state: &AppState, email: &str, new: &str) -> Result<()> {
        validate_password_strength(new).map_err(|m| AppError::Validation(m.into()))?;
        let row = {
            let conn = state.sqlite.conn()?;
            user::find_by_email(&conn, email)?
        }
        .ok_or_else(|| AppError::Validation("Error resetting password.".into()))?;
        Self::set_password(state, row.id, new)
    }

    fn set_password(state: &AppState, id: i64, new: &str) -> Result<()> {
        let hash = state.security.hash_password(new)?;
        {
            let conn = state.sqlite.conn()?;
            user::update_password_hash(&conn, id, &hash)?;
        }
        state.security.rewrap(new)?;
        state.sessions.clear();
        Ok(())
    }

    pub fn profile(
        state: &AppState,
        username: &str,
    ) -> Result<Option<(user::UserRow, Option<Secret>)>> {
        let row = {
            let conn = state.sqlite.conn()?;
            user::find_by_username(&conn, username)?
        };
        match row {
            Some(r) => {
                let s = r.totp_secret(&state.security)?;
                Ok(Some((r, s)))
            }
            None => Ok(None),
        }
    }

    /// Forgotten password with no authenticator: remove the account and
    /// everything tied to it, and replace the keys so nothing written before
    /// can be decrypted. Every credential that reaches this machine from
    /// outside stops working in the same transaction: MCP tokens are
    /// revoked, strategy and Chartink webhooks get new secret addresses
    /// (nobody holds them yet), and Telegram and WhatsApp links are removed.
    /// Strategy, Chartink and log history is kept.
    ///
    /// Only the desktop window calls this (the `reset_account` command,
    /// after a native confirmation); no HTTP route reaches it.
    pub fn reset_account(state: &AppState) -> Result<()> {
        {
            let conn = state.sqlite.conn()?;
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let now = state.now();
            let r = (|| -> Result<()> {
                conn.execute("DELETE FROM users", [])?;
                api_keys::delete_all(&conn)?;
                credentials::delete_all(&conn)?;
                auth::delete_all(&conn)?;
                conn.execute("DELETE FROM pending_oauth", [])?;
                crate::mcp::store::revoke_all(&conn, now)?;
                Self::rotate_webhook_secrets(&conn)?;
                conn.execute("DELETE FROM telegram_users", [])?;
                conn.execute(
                    "UPDATE bot_config SET token = NULL, is_active = 0 WHERE id = 1",
                    [],
                )?;
                conn.execute("DELETE FROM whatsapp_users", [])?;
                crate::messaging::whatsapp::db::clear_session(&conn)?;
                Ok(())
            })();
            match r {
                Ok(()) => conn.execute_batch("COMMIT")?,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }
        }
        state.security.rotate()?;
        state.api_keys.clear();
        state.sessions.clear();
        state.set_broker_session(None);
        Ok(())
    }

    /// New, unpublished secrets for every strategy webhook (only the digest
    /// is stored, so the new token is simply never shown) and every
    /// Chartink webhook id (the trader copies the new address from the page).
    fn rotate_webhook_secrets(conn: &rusqlite::Connection) -> Result<()> {
        let ids = |sql: &str| -> Result<Vec<i64>> {
            let mut st = conn.prepare(sql)?;
            let v = st
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        };
        for id in ids("SELECT id FROM sm_strategy")? {
            let hash = crate::strategy::store::hash_webhook_token(
                &crate::strategy::store::generate_webhook_token(),
            );
            conn.execute(
                "UPDATE sm_strategy SET webhook_token_hash = ?1 WHERE id = ?2",
                rusqlite::params![hash, id],
            )?;
        }
        for id in ids("SELECT id FROM chartink_strategies")? {
            conn.execute(
                "UPDATE chartink_strategies SET webhook_id = ?1 WHERE id = ?2",
                rusqlite::params![uuid::Uuid::new_v4().to_string(), id],
            )?;
        }
        Ok(())
    }

    /// The whole account reset as the desktop window runs it: end the live
    /// broker session and the bots, wipe and rotate (`reset_account`), then
    /// sign every window out.
    pub async fn reset_account_everywhere(ctx: &std::sync::Arc<AppState>) -> Result<()> {
        if let Err(e) = crate::services::broker_auth_service::BrokerAuthService::revoke(
            ctx,
            crate::events::SessionEndReason::Logout,
        )
        .await
        {
            tracing::warn!(
                "Account reset: the broker session could not be revoked: {}",
                e
            );
        }
        ctx.messaging.telegram.stop(ctx).await;
        ctx.messaging.whatsapp.stop_bot(ctx).await;
        let c = ctx.clone();
        tokio::task::spawn_blocking(move || Self::reset_account(&c))
            .await
            .map_err(|e| AppError::Internal(format!("account reset task: {}", e)))??;
        ctx.bus.publish(crate::events::Event::ForceLogout {
            message: "OpenAlgo was reset on this computer. Create a new account to continue."
                .into(),
        });
        tracing::info!("Account reset from the desktop window");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_policy_matches_web_messages() {
        assert_eq!(validate_password_strength(""), Err("Password is required"));
        assert_eq!(
            validate_password_strength("Ab1!"),
            Err("Password must be at least 8 characters long")
        );
        assert_eq!(
            validate_password_strength("abcdefg1!"),
            Err("Password must contain at least 1 uppercase letter (A-Z)")
        );
        assert_eq!(
            validate_password_strength("ABCDEFG1!"),
            Err("Password must contain at least 1 lowercase letter (a-z)")
        );
        assert_eq!(
            validate_password_strength("Abcdefgh!"),
            Err("Password must contain at least 1 number (0-9)")
        );
        assert_eq!(
            validate_password_strength("Abcdefgh1"),
            Err("Password must contain at least 1 special character (!@#$%^&*)")
        );
        assert!(validate_password_strength("Abcdefg1!").is_ok());
    }

    #[test]
    fn email_check() {
        assert!(valid_email("a@b.co"));
        assert!(!valid_email("ab.co"));
        assert!(!valid_email("a@b"));
    }
}
