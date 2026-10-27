//! The tunnel's settings: the switch, the authtoken (sealed with the data
//! key, never shown again after it is saved) and the reserved domain.

use super::provider::OpenRequest;
use super::status::{Off, TunnelStatus};
use crate::db::sqlite::tunnel::{self as store, StoredTunnel, TunnelUpdate};
use crate::error::{AppError, Result};
use crate::messaging::sealed;
use crate::security::Secret;
use crate::state::AppState;
use serde::Serialize;
use std::sync::Arc;

/// What the settings page shows. The authtoken itself never leaves Rust:
/// only whether one is saved.
#[derive(Debug, Serialize)]
pub struct TunnelView {
    pub enabled: bool,
    pub authtoken_saved: bool,
    pub domain: Option<String>,
    pub tunnel: TunnelStatus,
}

/// One save from the settings page. `None` keeps a field.
#[derive(Debug, Default)]
pub struct TunnelChange {
    pub enabled: Option<bool>,
    pub authtoken: Option<Secret>,
    /// Remove the saved authtoken (the tunnel stops).
    pub forget_authtoken: bool,
    /// `Some("")` clears the domain.
    pub domain: Option<String>,
}

const BAD_AUTHTOKEN: &str = "That does not look like an ngrok authtoken. Copy it from Your \
Authtoken in your ngrok dashboard and paste it here.";
const BAD_DOMAIN: &str = "Enter the domain as it appears under Domains in your ngrok dashboard, \
for example yourname.ngrok-free.app.";

/// An authtoken as ngrok issues them: letters, digits, `_` and `-`. The
/// value is never repeated in the answer.
fn check_authtoken(token: &Secret) -> Result<()> {
    let t = token.expose();
    let ok = (10..=256).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(AppError::Validation(BAD_AUTHTOKEN.into()))
    }
}

/// The domain as ngrok takes it: a host name, lower case. A pasted
/// address (`https://name.ngrok-free.app/`) is reduced to its host;
/// anything with a path, port, query or user name is refused. Empty
/// clears the domain.
fn normalize_domain(input: &str) -> Result<Option<String>> {
    let v = input.trim();
    if v.is_empty() {
        return Ok(None);
    }
    let bad = || AppError::Validation(BAD_DOMAIN.into());
    let host = if v.contains("://") {
        if !crate::config::url_with_scheme(v, &["http", "https"], false) {
            return Err(bad());
        }
        let u = url::Url::parse(v).map_err(|_| bad())?;
        if u.port().is_some() {
            return Err(bad());
        }
        u.host_str().ok_or_else(bad)?.to_string()
    } else {
        v.to_string()
    };
    let host = host.to_ascii_lowercase();
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    let labels: Vec<&str> = host.split('.').collect();
    if host.len() > 253 || labels.len() < 2 || !labels.iter().all(|l| label_ok(l)) {
        return Err(bad());
    }
    Ok(Some(host))
}

/// The settings as stored, with nothing opened.
fn stored(state: &AppState) -> Result<StoredTunnel> {
    let conn = state.sqlite.conn()?;
    store::read(&conn)
}

/// The settings page's view.
pub async fn view(ctx: &Arc<AppState>) -> Result<TunnelView> {
    let s = ctx.blocking(stored).await?;
    Ok(TunnelView {
        enabled: s.enabled,
        authtoken_saved: s.authtoken.is_some(),
        domain: s.domain,
        tunnel: ctx.tunnel.status(),
    })
}

/// Check and store `change`. Nothing is stored when a field is refused.
pub(super) async fn store_change(ctx: &Arc<AppState>, change: TunnelChange) -> Result<()> {
    if let Some(t) = &change.authtoken {
        check_authtoken(t)?;
    }
    let domain = change.domain.as_deref().map(normalize_domain).transpose()?;
    ctx.blocking(move |state| {
        let authtoken = match (&change.authtoken, change.forget_authtoken) {
            (Some(t), _) => Some(Some(sealed::seal(
                &state.security,
                &store::authtoken_aad(),
                t.expose(),
            )?)),
            (None, true) => Some(None),
            (None, false) => None,
        };
        let mut conn = state.sqlite.conn()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        store::save(
            &tx,
            &TunnelUpdate {
                enabled: change.enabled,
                authtoken,
                domain,
            },
        )?;
        store::pin_host_server(&tx)?;
        tx.commit()?;
        Ok(())
    })
    .await?;
    // The switch is also `ngrok_allow`, and the Host Server URL may now be
    // the domain: both are read by the redirect and webhook addresses.
    ctx.reload_config()?;
    Ok(())
}

/// What the tunnel should be doing now: open with this request, or
/// stopped for this reason. The authtoken opens only while the keys are
/// in memory (signed in, KEY-01).
pub(super) async fn wanted(ctx: &Arc<AppState>) -> std::result::Result<OpenRequest, Off> {
    let s = match ctx.blocking(stored).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Could not read the tunnel settings: {}", e);
            return Err(Off::SettingsUnreadable);
        }
    };
    if !s.enabled {
        return Err(Off::Disabled);
    }
    if !ctx.security.is_unlocked() {
        return Err(Off::SignedOut);
    }
    let sealed_token = s.authtoken.ok_or(Off::NoAuthtoken)?;
    let domain = s.domain.ok_or(Off::NoDomain)?;
    let authtoken =
        sealed::open(&ctx.security, &store::authtoken_aad(), &sealed_token).map_err(|e| {
            tracing::error!("The saved ngrok authtoken could not be opened: {}", e);
            Off::Unreadable
        })?;
    Ok(OpenRequest { authtoken, domain })
}
