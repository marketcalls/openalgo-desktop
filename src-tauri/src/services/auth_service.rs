//! OpenAlgo account: setup, sign-in, password change and reset, account reset.
//! Mirrors `blueprints/core.py` and `blueprints/auth.py` of the web.

use crate::db::sqlite::{api_keys, auth, credentials, user};
use crate::error::{AppError, Result};
use crate::security::{totp, Opened, Secret, SecurityManager, SignInFrom, Unlocked};
use crate::state::AppState;

/// The answer to a wrong current password on a password change.
pub const WRONG_CURRENT_PASSWORD: &str = "Current password is incorrect";

/// The answer when another password change finished first.
pub const PASSWORD_CHANGED_MEANWHILE: &str =
    "Your password was changed while this change was being saved. Sign in with the new password and try again.";

/// How many stand-in password checks ran (tests read it to see the
/// unknown-name path ran the same work).
pub use crate::security::hashing::STAND_IN_CHECKS;

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

/// What a password sign-in came to. The keys the password opened travel
/// with it: they are installed only with the signed-in session
/// (`SecurityManager::install`, inside `issue_if_current`), so a sign-in
/// still waiting for its authenticator code unlocks nothing (KEY-01).
///
/// Only `Success` and `TotpRequired` prove the password. The route answers
/// every other outcome with "Invalid credentials" to anyone but the app
/// window (KEY-01 security review); see `Opened` for what each means.
#[derive(Debug)]
pub enum LoginOutcome {
    Success(String, Unlocked),
    TotpRequired(String, Unlocked),
    Invalid,
    /// An earlier build's keys still have to move: only a sign-in in the
    /// app window moves them. The same for every name and password.
    MoveInAppWindow,
    /// The keys cannot be read, whatever was typed.
    Unreadable(AppError),
    /// The password was proven, then something after it failed.
    FailedAfterProof(AppError),
}

impl LoginOutcome {
    /// Whether the password was proven and a session may follow.
    pub fn signed_in(&self) -> bool {
        matches!(
            self,
            LoginOutcome::Success(..) | LoginOutcome::TotpRequired(..)
        )
    }
    /// What the caller of a sign-in that issued no session is told
    /// (KEY-01 security review): the app window hears why the keys could
    /// not be opened, with the ways out; anyone else the answer to a wrong
    /// password, so no failure says whether a name or a password was right.
    pub fn refusal(self, from: SignInFrom) -> Refusal {
        match self {
            LoginOutcome::MoveInAppWindow => Refusal::MoveInAppWindow,
            LoginOutcome::Unreadable(e) | LoginOutcome::FailedAfterProof(e)
                if from == SignInFrom::AppWindow =>
            {
                Refusal::Explain(e)
            }
            _ => Refusal::InvalidCredentials,
        }
    }
}

/// How a sign-in that issued no session is answered.
#[derive(Debug)]
pub enum Refusal {
    /// "Invalid credentials", counted as a failure: every failure seen
    /// from anywhere but the app window.
    InvalidCredentials,
    /// An earlier build's keys still have to move, in the app window; the
    /// same for every name and password, counted as a failure.
    MoveInAppWindow,
    /// The app window is told why, with the ways out; nothing counts.
    Explain(AppError),
    /// The app window is asked to try again: the sign-in could not run to
    /// its end. Nothing counts.
    TryAgain,
}

impl Refusal {
    /// The answer when the sign-in itself could not run to its end.
    pub fn sign_in_broke(from: SignInFrom) -> Self {
        match from {
            SignInFrom::AppWindow => Refusal::TryAgain,
            SignInFrom::Elsewhere => Refusal::InvalidCredentials,
        }
    }
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
        // The keys, their vault, the account and its API key in one
        // transaction (KEY-01). Creating the account counts as its first
        // sign-in: the keys stay in memory until the trader logs out.
        let secret = totp::generate_secret();
        let key = api_keys::generate_api_key();
        state
            .security
            .create_account(&state.sqlite, password, |conn, with| {
                let hash = with.hash_password(password)?;
                user::insert(conn, with, username, email.trim(), &hash, &secret)?;
                api_keys::upsert_for_user(conn, with, username, &key)?;
                Ok(())
            })?;
        state.api_keys.clear();
        tracing::info!("Account created");
        Ok(())
    }

    /// Check a username and password. Does not create a web session and
    /// installs no keys: the keys the password opened come back in the
    /// outcome, for the caller to install with the session.
    ///
    /// The first sign-in in the app window of an install from an earlier
    /// build moves its keys into openalgo.db
    /// (`SecurityManager::open_for_sign_in`): from the OS keychain (the one
    /// last read), `vault.json` or `secrets.dat`. Every way it can fail, and
    /// how much a caller may be told, is in `LoginOutcome`.
    pub fn verify_credentials(
        state: &AppState,
        username: &str,
        password: &str,
        from: SignInFrom,
    ) -> LoginOutcome {
        #[cfg(test)]
        test_hooks::after_account_read(state);
        // The password must open the vault and match the stored hash, read
        // under the lock a password change holds (SEC-02). Every way out
        // before that costs the same work and says nothing about the name
        // or the password (KEY-01 security review).
        let keys = match state
            .security
            .open_for_sign_in(&state.sqlite, username, password, from)
        {
            Opened::Keys(k) => k,
            Opened::WrongPassword => return LoginOutcome::Invalid,
            Opened::MoveInAppWindow => return LoginOutcome::MoveInAppWindow,
            Opened::Unreadable(e) => return LoginOutcome::Unreadable(e),
            Opened::FailedAfterProof(e) => return LoginOutcome::FailedAfterProof(e),
        };
        match Self::after_proof(state, username, &keys) {
            Ok((name, true)) => LoginOutcome::TotpRequired(name, keys),
            Ok((name, false)) => LoginOutcome::Success(name, keys),
            Err(e) => {
                tracing::error!("Sign-in failed after the password was checked: {}", e);
                LoginOutcome::FailedAfterProof(e)
            }
        }
    }

    /// The steps of a sign-in once the password is proven: rows an early
    /// build stored without associated data are re-encrypted (a no-op once
    /// done), `secrets.dat` goes, and an account from an earlier desktop
    /// build gets its TOTP secret. Returns the account's name and whether
    /// it needs the authenticator code.
    fn after_proof(state: &AppState, username: &str, keys: &Unlocked) -> Result<(String, bool)> {
        let row = {
            let conn = state.sqlite.conn()?;
            user::find_by_username(&conn, username)?
        }
        .ok_or_else(|| AppError::Internal("the account went away during its sign-in".into()))?;
        let with = keys.manager();
        crate::db::sqlite::data_migrations::run(&state.sqlite, &with)?;
        state.security.finish_legacy_migration(&state.sqlite)?;
        Self::ensure_totp_secret(state, &with, &row)?;
        let totp = row.is_totp_required_for("login");
        Ok((row.username, totp))
    }

    /// Accounts created by earlier desktop builds have no TOTP secret.
    fn ensure_totp_secret(
        state: &AppState,
        with: &SecurityManager,
        row: &user::UserRow,
    ) -> Result<()> {
        if row.totp_secret(with)?.is_none() {
            let conn = state.sqlite.conn()?;
            user::set_totp_secret(&conn, with, &row.username, &totp::generate_secret())?;
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

    /// The authenticator code of a sign-in still waiting for it, checked
    /// with the keys its password opened (not installed yet).
    pub fn verify_totp_with(
        state: &AppState,
        username: &str,
        code: &str,
        keys: &Unlocked,
    ) -> Result<bool> {
        let row = {
            let conn = state.sqlite.conn()?;
            user::find_by_username(&conn, username)?
        };
        let Some(row) = row else { return Ok(false) };
        let Some(secret) = row.totp_secret(&keys.manager())? else {
            return Ok(false);
        };
        let now = state.now().timestamp().max(0) as u64;
        Ok(totp::verify(secret.expose(), code, now))
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
        Self::set_password(state, row.id, &row.password_hash, new)
    }

    /// Store a new password: the hash and the keys wrapped with it, in one
    /// transaction (KEY-01; `SecurityManager::change_password`), so there is
    /// no state in which one changed and the other did not.
    ///
    /// `old_hash` is the hash the caller checked the request against. The
    /// change applies only while it is still the stored one, checked in the
    /// same statement that writes the new hash, under the vault lock: a
    /// second change validated against the same old password, finishing
    /// after the first, is refused instead of overwriting it (SEC-02b).
    /// Every browser session ends and every cache is cleared after the
    /// commit; the keys do not change, so they stay in memory.
    pub(crate) fn set_password(state: &AppState, id: i64, old_hash: &str, new: &str) -> Result<()> {
        state
            .security
            .change_password(&state.sqlite, id, old_hash, new, || {
                AppError::Validation(PASSWORD_CHANGED_MEANWHILE.into())
            })?;
        state.sessions.clear();
        Self::clear_caches(state);
        // No copy of the old password hash or vault may outlive the change
        // (REL-02 security review).
        crate::db::upgrade::purge_backups(&state.data_dir, "Password changed");
        Ok(())
    }

    /// Every cache a change of credentials makes stale (the web's
    /// `invalidate_user_cache`): API keys and order mode, instrument facts.
    /// Run after the change is committed.
    pub fn clear_caches(state: &AppState) {
        state.api_keys.clear();
        state.trading.facts.clear();
    }

    /// A new password without the old one, which starts fresh: new keys
    /// wrapped with the new password, every value the old keys encrypted
    /// dropped (saved broker keys, stored broker sessions, the Telegram bot
    /// token and linked users' keys, the WhatsApp pairing), and a new API
    /// key (order mode kept) and TOTP secret (every "require the
    /// authenticator" switch off), in one transaction. The account, its
    /// settings and its history stay. The keys are locked afterwards: the
    /// trader signs in with the new password.
    ///
    /// Used for a forgotten password and for an install whose earlier
    /// build's keys cannot be read. Only the desktop window calls it (the
    /// `start_fresh` command, after a native confirmation); no HTTP route
    /// reaches it.
    pub fn start_fresh(state: &AppState, new: &str, confirm: &str) -> Result<()> {
        Self::check_new_password(new, confirm)?;
        let row = {
            let conn = state.sqlite.conn()?;
            user::find_first(&conn)?
        }
        .ok_or_else(|| {
            AppError::Validation(
                "There is no account on this computer yet. Create one instead.".into(),
            )
        })?;
        let secret = totp::generate_secret();
        let key = api_keys::generate_api_key();
        state
            .security
            .replace_keys(&state.sqlite, new, |conn, with| {
                let hash = with.hash_password(new)?;
                conn.execute(
                "UPDATE users SET password_hash = ?1, updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![hash, row.id],
            )?;
                user::set_totp_secret(conn, with, &row.username, &secret)?;
                api_keys::upsert_for_user(conn, with, &row.username, &key)?;
                Ok(())
            })?;
        // No copy of the old keys and what they protected may outlive them
        // in the data folder's update backups (REL-02 security review).
        crate::db::upgrade::purge_backups(&state.data_dir, "Fresh start");
        // The stored broker sessions are gone, so nothing is left to revoke.
        crate::services::broker_auth_service::BrokerAuthService::clear_pending_revoke(state);
        state.sessions.clear();
        state.set_broker_session(None);
        Self::clear_caches(state);
        tracing::info!("Started fresh with a new password; the old encrypted values were dropped");
        Ok(())
    }

    /// What a new password set without the old one must pass before
    /// anything changes: the two entries match, and the web's strength
    /// rules hold.
    pub fn check_new_password(new: &str, confirm: &str) -> Result<()> {
        if new != confirm {
            return Err(AppError::Validation("The passwords do not match.".into()));
        }
        validate_password_strength(new).map_err(|m| AppError::Validation(m.into()))
    }

    /// The fresh start as the desktop window runs it: end the live broker
    /// session and the bots, start fresh, clear every adapter's state, then
    /// sign every window out.
    pub async fn start_fresh_everywhere(
        ctx: &std::sync::Arc<AppState>,
        new: &str,
        confirm: &str,
    ) -> Result<()> {
        Self::check_new_password(new, confirm)?;
        ctx.agent.stop_all();
        ctx.flow.stop_all(crate::flow::STOP_GRACE).await;
        if let Err(e) = crate::services::broker_auth_service::BrokerAuthService::revoke(
            ctx,
            crate::events::SessionEndReason::Logout,
        )
        .await
        {
            tracing::warn!(
                "Fresh start: the broker session ended; its stored row could not be revoked: {}",
                e
            );
        }
        ctx.messaging.telegram.stop(ctx).await;
        ctx.messaging.whatsapp.stop_bot(ctx).await;
        ctx.tunnel
            .stop(crate::services::tunnel::Off::SignedOut)
            .await;
        let c = ctx.clone();
        let (n, f) = (new.to_string(), confirm.to_string());
        tokio::task::spawn_blocking(move || Self::start_fresh(&c, &n, &f))
            .await
            .map_err(|e| AppError::Internal(format!("fresh start task: {}", e)))??;
        Self::forget_adapter_state(ctx).await;
        ctx.bus.publish(crate::events::Event::ForceLogout {
            message: "A new password was set on this computer. Sign in with it.".into(),
        });
        Ok(())
    }

    /// Log out: after the broker session was revoked, lock the keys, stop
    /// the bots, and clear every cache and every adapter's in-memory state
    /// (KEY-01). Until the next sign-in nothing can be decrypted.
    ///
    /// The keys go together with every browser session, under the lock a
    /// sign-in issues its session under (`SecurityManager::lock_with`): a
    /// sign-in checked before this point gets no session, and one that
    /// finished during the revoke does not keep a session without keys.
    /// The bots end as at an app exit, keeping their "on" setting, so the
    /// next sign-in brings them back (`messaging::autostart`).
    pub async fn lock_everything(ctx: &std::sync::Arc<AppState>) {
        ctx.security.lock_with(|| ctx.sessions.clear());
        ctx.agent.stop_all();
        ctx.flow.stop_all(crate::flow::STOP_GRACE).await;
        ctx.messaging.shutdown().await;
        ctx.tunnel
            .stop(crate::services::tunnel::Off::SignedOut)
            .await;
        Self::clear_caches(ctx);
        ctx.clear_symbol_cache();
        Self::forget_adapter_state(ctx).await;
        tracing::info!("Keys locked until the next sign-in");
    }

    /// `on_logout` of every broker adapter, not only the logged-in one: an
    /// adapter a sign-in or resume touched but never completed may still
    /// hold keys or tokens (XTS market keys, OTP tokens).
    async fn forget_adapter_state(ctx: &AppState) {
        for b in ctx.brokers.list() {
            b.on_logout().await;
        }
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

    /// The id of the account `username`, if there is one.
    pub fn user_id(state: &AppState, username: &str) -> Result<Option<i64>> {
        let conn = state.sqlite.conn()?;
        Ok(user::find_by_username(&conn, username)?.map(|r| r.id))
    }

    /// Save the two-factor choices of the account `user_id`.
    pub fn set_two_factor(state: &AppState, user_id: i64, f: user::TwoFactorFlags) -> Result<()> {
        let conn = state.sqlite.conn()?;
        user::set_two_factor(&conn, user_id, f)
    }

    /// Forgotten password with no authenticator: remove the account and
    /// everything tied to it, and replace the keys so nothing written before
    /// can be decrypted. Every credential that reaches this machine from
    /// outside stops working in the same transaction: MCP tokens are
    /// revoked, strategy, Chartink and Flow webhooks get new secret addresses
    /// (nobody holds them yet; every Flow workflow is stopped), and Telegram
    /// and WhatsApp links are removed.
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
                crate::flow::store::reset_credentials(&conn)?;
                conn.execute("DELETE FROM telegram_users", [])?;
                conn.execute(
                    "UPDATE bot_config SET token = NULL, is_active = 0 WHERE id = 1",
                    [],
                )?;
                conn.execute("DELETE FROM whatsapp_users", [])?;
                crate::messaging::whatsapp::db::clear_session(&conn)?;
                crate::db::sqlite::tunnel::forget_authtoken(&conn)?;
                crate::agent::models::secrets::wipe(&conn)?;
                // The keys go with the account; the next setup makes new
                // ones (KEY-01).
                crate::db::sqlite::key_vault::delete(&conn)?;
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
        // No copy of the old account may outlive it in the data folder's
        // update backups (REL-02 security review).
        crate::db::upgrade::purge_backups(&state.data_dir, "Account reset");
        // The stored broker sessions are gone, so nothing is left to revoke.
        crate::services::broker_auth_service::BrokerAuthService::clear_pending_revoke(state);
        // The account and its keys are gone: the keys are locked, every
        // window is signed out and nothing cached survives.
        state.security.forget_keys();
        Self::clear_caches(state);
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
    /// broker session (its streaming always stops, even when the stored row
    /// cannot be written) and the bots, wipe and rotate (`reset_account`,
    /// which deletes the stored row), then sign every window out.
    pub async fn reset_account_everywhere(ctx: &std::sync::Arc<AppState>) -> Result<()> {
        ctx.agent.stop_all();
        ctx.flow.stop_all(crate::flow::STOP_GRACE).await;
        // Every schedule, alert and watch goes, with the workflows' feed
        // subscriptions; the reset below stands every workflow down (D-19).
        ctx.flow.triggers().reset().await;
        if let Err(e) = crate::services::broker_auth_service::BrokerAuthService::revoke(
            ctx,
            crate::events::SessionEndReason::Logout,
        )
        .await
        {
            tracing::warn!(
                "Account reset: the broker session ended; its stored row could not be revoked: {}",
                e
            );
        }
        ctx.messaging.telegram.stop(ctx).await;
        ctx.messaging.whatsapp.stop_bot(ctx).await;
        ctx.tunnel
            .stop(crate::services::tunnel::Off::SignedOut)
            .await;
        let c = ctx.clone();
        tokio::task::spawn_blocking(move || Self::reset_account(&c))
            .await
            .map_err(|e| AppError::Internal(format!("account reset task: {}", e)))??;
        Self::forget_adapter_state(ctx).await;
        ctx.bus.publish(crate::events::Event::ForceLogout {
            message: "OpenAlgo was reset on this computer. Create a new account to continue."
                .into(),
        });
        tracing::info!("Account reset from the desktop window");
        Ok(())
    }
}

/// Test hooks: stop one app state's next sign-in right after it read the
/// account, so a test can run a password change in between.
#[cfg(test)]
pub(crate) mod test_hooks {
    use crate::state::AppState;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Mutex;

    struct Pause {
        state: usize,
        reached: Sender<()>,
        release: Receiver<()>,
    }

    static PAUSE: Mutex<Option<Pause>> = Mutex::new(None);

    /// Arm the pause for `state`: the returned receiver fires when its next
    /// sign-in has read the account, and the sign-in goes on once the
    /// returned sender sends.
    pub(crate) fn pause_next_sign_in(state: &AppState) -> (Receiver<()>, Sender<()>) {
        let (reached_tx, reached_rx) = channel();
        let (release_tx, release_rx) = channel();
        *PAUSE.lock().unwrap() = Some(Pause {
            state: state as *const AppState as usize,
            reached: reached_tx,
            release: release_rx,
        });
        (reached_rx, release_tx)
    }

    pub(super) fn after_account_read(state: &AppState) {
        let pause = {
            let mut p = PAUSE.lock().unwrap();
            match p.as_ref() {
                Some(x) if x.state == state as *const AppState as usize => p.take(),
                _ => None,
            }
        };
        if let Some(p) = pause {
            let _ = p.reached.send(());
            let _ = p.release.recv();
        }
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

    /// SEC-02b, KEY-01: a password change (the hash and the wrapped keys,
    /// one transaction) is written with the database's full sync, so it is
    /// on disk when the change returns, and the connection's own setting
    /// comes back afterwards.
    #[test]
    fn the_password_hash_is_written_with_full_sync() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("x.db")).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .unwrap();
        let sync = |c: &rusqlite::Connection| -> i64 {
            c.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap()
        };
        let inside = crate::security::with_full_sync(&conn, || Ok(sync(&conn))).unwrap();
        assert_eq!(inside, 2, "FULL while the hash is written");
        assert_eq!(sync(&conn), 1, "NORMAL again afterwards");
        // An error inside still restores the setting.
        let r: Result<()> =
            crate::security::with_full_sync(&conn, || Err(AppError::Validation("x".into())));
        assert!(r.is_err());
        assert_eq!(sync(&conn), 1);
    }
}
