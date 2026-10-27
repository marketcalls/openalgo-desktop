//! The web-shaped broker settings (`/api/broker/credentials`, web
//! `blueprints/broker_credentials.py`): the active broker, its keys, the
//! redirect URL and the server addresses in one save, and the masked view of
//! the active broker's keys.
//!
//! The Broker Configuration page saves one broker at a time through
//! `broker_config_service`; both paths store keys with
//! [`BrokerConfigService::save_keys`].

use crate::config::{ServerConfig, ServerConfigUpdate};
use crate::db::sqlite::credentials::{self, CredentialUpdate, MaskedCredentials};
use crate::error::{AppError, Result};
use crate::events::SessionEndReason;
use crate::services::broker_auth_service::BrokerAuthService;
use crate::services::broker_config_service::BrokerConfigService;
use crate::state::AppState;
use tokio::sync::MutexGuard;

/// One save of the settings form. Every `None` keeps what is stored.
#[derive(Debug, Default)]
pub struct SettingsSave {
    /// The broker the save names, already checked against the catalogue. A
    /// save that names none never writes the active broker: a snapshot
    /// taken by the caller could be stale by the time it is written
    /// (FLOW-03).
    pub broker: Option<String>,
    pub update: CredentialUpdate,
    pub redirect_url: Option<String>,
    /// `Some("")` goes back to the automatic address.
    pub host_server: Option<String>,
    /// `Some("")` goes back to the automatic address.
    pub websocket_url: Option<String>,
    pub ngrok_allow: Option<bool>,
}

impl SettingsSave {
    fn has_keys(&self) -> bool {
        let u = &self.update;
        u.api_key.is_some()
            || u.api_secret.is_some()
            || u.api_key_market.is_some()
            || u.api_secret_market.is_some()
            || u.client_id.is_some()
    }
}

/// What a save did.
#[derive(Debug, PartialEq, Eq)]
pub enum SettingsOutcome {
    /// Saved. `switched` when the save made another broker the active one;
    /// `signed_out_of` names the broker whose session it ended.
    Saved {
        switched: bool,
        signed_out_of: Option<String>,
    },
    /// The save would end a session that Live automation or unsettled
    /// orders still depend on; the reason, for the trader. Nothing was
    /// saved.
    Refused(String),
}

/// Why a save failed. Nothing was stored either way.
#[derive(Debug)]
pub enum SettingsError {
    /// The stored settings could not be read to check the live session.
    Read(AppError),
    /// The save itself failed; `AppError::Validation` carries the trader's
    /// message.
    Save(AppError),
}

/// Whether a save ends the live session of `live`: it makes another broker
/// the active one, or it carries new keys for `live` (FLOW-04: a session
/// belongs to the keys it signed in with). `target` is the broker the keys
/// go to.
fn ends_session(live: &str, broker: Option<&str>, has_keys: bool, target: Option<&str>) -> bool {
    broker.is_some_and(|b| b != live) || (has_keys && target == Some(live))
}

/// Save the settings. One lifecycle lock covers the save and the sign-out
/// it causes, and the settings are read again inside the transaction: two
/// saves, or a save and a sign-in, never interleave, and "switched" is
/// decided against what is stored now, not a snapshot (FLOW-03).
pub async fn save(
    state: &AppState,
    req: SettingsSave,
) -> std::result::Result<SettingsOutcome, SettingsError> {
    let has_keys = req.has_keys();
    let broker = req.broker.clone();
    let tunnel_switched = req.ngrok_allow.is_some();
    let held = BrokerAuthService::session_lock(state).await;
    if let Some(why) = live_session_refusal(state, broker.as_deref(), has_keys)? {
        return Ok(SettingsOutcome::Refused(why));
    }
    let (previous, target) = store(state, req, has_keys).map_err(SettingsError::Save)?;
    let _ = state.reload_config();
    let switched = broker.is_some() && broker != previous;
    let signed_out_of =
        end_replaced_session(state, broker.as_deref(), has_keys, target.as_deref(), &held).await;
    drop(held);
    // The web's NGROK_ALLOW is the built-in tunnel's switch.
    if let Some(ctx) = state.arc().filter(|_| tunnel_switched) {
        crate::services::tunnel::apply(&ctx, false).await;
    }
    Ok(SettingsOutcome::Saved {
        switched,
        signed_out_of,
    })
}

/// A save that would end the live session (a switch, or new keys for the
/// logged-in broker) is refused while Live automation or unsettled orders
/// depend on it, before anything is stored.
fn live_session_refusal(
    state: &AppState,
    broker: Option<&str>,
    has_keys: bool,
) -> std::result::Result<Option<String>, SettingsError> {
    let Some(live) = state.get_broker_session() else {
        return Ok(None);
    };
    let stored_active = state
        .sqlite
        .conn()
        .and_then(|c| ServerConfig::load(&c))
        .map_err(SettingsError::Read)?
        .active_broker;
    let target = broker.map(str::to_string).or(stored_active);
    if !ends_session(&live.broker_id, broker, has_keys, target.as_deref()) {
        return Ok(None);
    }
    Ok(crate::services::broker_switch::refusal(
        state,
        &live.broker_id,
    ))
}

/// Store the keys and the settings in one transaction. Answers the active
/// broker before the save and the broker the keys went to (the one the
/// save names, else the active one).
fn store(
    state: &AppState,
    req: SettingsSave,
    has_keys: bool,
) -> Result<(Option<String>, Option<String>)> {
    let mut c = state.sqlite.conn()?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let previous = ServerConfig::load(&tx)?.active_broker;
    let target = req.broker.clone().or_else(|| previous.clone());
    if has_keys {
        let b = target.clone().ok_or_else(|| {
            AppError::Validation("Choose your broker before saving its API key.".into())
        })?;
        if let Some(k) = &req.update.api_key {
            BrokerConfigService::check_api_key_format(&b, k.expose())
                .map_err(|m| AppError::Validation(m.into()))?;
        }
        // Saving the settings again is how a trader switches accounts: the
        // next sign-in is no longer bound to the last one's.
        BrokerConfigService::save_keys(&tx, &state.security, &b, req.update)?;
    }
    crate::config::save(
        &tx,
        &ServerConfigUpdate {
            // Only when the save names a broker.
            active_broker: req.broker,
            redirect_url: req.redirect_url,
            host_server: req.host_server,
            websocket_url: req.websocket_url,
            ngrok_allow: req.ngrok_allow,
            ..Default::default()
        },
    )?;
    // The switch on with an ngrok domain saved: the Host Server URL is the
    // domain, whatever the form sent (`db::sqlite::tunnel::pin_host_server`).
    crate::db::sqlite::tunnel::pin_host_server(&tx)?;
    tx.commit()?;
    Ok((previous, target))
}

/// One broker at a time: a save that makes another broker the active one
/// ends the live session of the old one, and new keys for the logged-in
/// broker end its session too (FLOW-04). Answers the broker signed out of.
async fn end_replaced_session(
    state: &AppState,
    broker: Option<&str>,
    has_keys: bool,
    target: Option<&str>,
    held: &MutexGuard<'_, ()>,
) -> Option<String> {
    let s = state.get_broker_session()?;
    if !ends_session(&s.broker_id, broker, has_keys, target) {
        return None;
    }
    // The session ends in memory whatever the database does; a failed write
    // of its stored row is retried by the session poll.
    if let Err(e) = BrokerAuthService::revoke_held(state, SessionEndReason::Logout, held).await {
        tracing::error!(
            "Broker session ended by the settings save; its stored row is still to be revoked: {}",
            e
        );
    }
    Some(s.broker_id)
}

/// The saved keys of `broker`, masked, and its client id (an account id,
/// not a secret, which the Profile form shows). No broker, or nothing
/// saved, reads as empty.
pub fn masked(state: &AppState, broker: &str) -> Result<(MaskedCredentials, Option<String>)> {
    if broker.is_empty() {
        return Ok((MaskedCredentials::default(), None));
    }
    let conn = state.sqlite.conn()?;
    Ok(credentials::load(&conn, &state.security, broker)?
        .map(|s| (s.masked(), s.client_id))
        .unwrap_or_default())
}

/// Every broker with saved keys.
pub fn configured(state: &AppState) -> Result<Vec<String>> {
    let conn = state.sqlite.conn()?;
    credentials::list_configured(&conn)
}

#[cfg(test)]
mod tests {
    use super::ends_session;

    #[test]
    fn a_save_ends_the_live_session_only_by_switching_or_with_new_keys() {
        // Another broker named: a switch.
        assert!(ends_session("zerodha", Some("dhan"), false, Some("dhan")));
        // New keys for the logged-in broker.
        assert!(ends_session("zerodha", None, true, Some("zerodha")));
        assert!(ends_session(
            "zerodha",
            Some("zerodha"),
            true,
            Some("zerodha")
        ));
        // The same broker named again without keys, or keys for another.
        assert!(!ends_session(
            "zerodha",
            Some("zerodha"),
            false,
            Some("zerodha")
        ));
        assert!(!ends_session("zerodha", None, true, Some("dhan")));
        // Addresses only.
        assert!(!ends_session("zerodha", None, false, Some("zerodha")));
    }
}
