//! The credential a plan request sends: the stored access token, renewed
//! first when it is within a minute of expiring, and the account header
//! (LiteLLM's `Authenticator.get_access_token` and `get_account_id`, which
//! the web leans on).
//!
//! D-025: the web's own expiry check has no skew; LiteLLM renewed 60 s
//! early behind it. The desktop has no LiteLLM, so the renewal here uses
//! that 60 s skew itself, and a turn never starts on a token that expires
//! mid-stream. A rotated refresh token is kept in the vault in the same
//! step. One renewal runs at a time.
//!
//! Nothing here starts a sign-in: with no usable credential the request is
//! refused with a sentence before anything is sent.

use super::flow::{self, RenewFailure};
use super::record::{self, AuthRecord};
use super::signin::{unix, SignIn};
use crate::agent::contract::model::{ModelError, ProviderAuth, ProviderCredentials};
use crate::security::Secret;
use crate::state::AppState;
use async_trait::async_trait;
use std::sync::{Arc, Weak};

/// Renew this many seconds before the access token expires (LiteLLM's
/// `TOKEN_EXPIRY_SKEW_SECONDS`).
pub const SKEW_SECONDS: f64 = 60.0;

/// The web's plan override for a refused sign-in (`error_messages.py`).
pub const SIGN_IN_AGAIN: &str =
    "Your ChatGPT sign-in has expired or was removed. Sign in to ChatGPT again from agent settings.";
pub const RENEW_UNREACHABLE: &str = "Could not reach ChatGPT's sign-in service to renew the sign-in. Check the internet connection and try again.";
pub const RENEW_INCOMPLETE: &str = "ChatGPT sent back an incomplete credential while renewing the sign-in. Sign in to ChatGPT again from agent settings.";

/// An access token ready to send, with its account.
pub struct Access {
    pub token: Secret,
    pub account_id: Option<String>,
}

fn account_of(record: &AuthRecord) -> Option<String> {
    record.account_id.clone().or_else(|| {
        record
            .id_token
            .as_ref()
            .and_then(|t| record::account_claim(t.expose()))
            .or_else(|| record.access().and_then(record::account_claim))
    })
}

impl SignIn {
    /// The access token a request sends now, renewed first when it is
    /// within [`SKEW_SECONDS`] of expiring or missing.
    pub async fn access(&self, app: &Arc<AppState>) -> Result<Access, ModelError> {
        let _one = self.renewal.lock().await;
        let stored = app
            .blocking(|c| Ok(record::load(&*c.sqlite.conn()?, &c.security)))
            .await
            .ok()
            .flatten();
        let Some(record) = stored else {
            return Err(ModelError::MissingCredential(
                crate::agent::plan::NOT_SIGNED_IN.into(),
            ));
        };
        let now = unix(app);
        if let Some(token) = record
            .access()
            .filter(|_| !record.access_expired(now, SKEW_SECONDS))
        {
            return Ok(Access {
                token: Secret::new(token),
                account_id: account_of(&record),
            });
        }
        let Some(refresh) = record.refresh() else {
            return Err(ModelError::MissingCredential(SIGN_IN_AGAIN.into()));
        };
        let tokens = flow::renew(&self.flow.endpoints, refresh)
            .await
            .map_err(|f| {
                ModelError::MissingCredential(
                    match f {
                        RenewFailure::Refused => SIGN_IN_AGAIN,
                        RenewFailure::Incomplete => RENEW_INCOMPLETE,
                        RenewFailure::Unreachable => RENEW_UNREACHABLE,
                    }
                    .into(),
                )
            })?;
        let kept = tokens.refresh.or_else(|| record.refresh_token.clone());
        let renewed = AuthRecord::from_tokens(tokens.access, kept, tokens.id);
        let token = Secret::new(renewed.access().unwrap_or_default());
        let account_id = account_of(&renewed);
        let saved = app
            .blocking(move |c| Ok(record::store(&*c.sqlite.conn()?, &c.security, &renewed).is_ok()))
            .await
            .unwrap_or(false);
        if saved {
            tracing::info!("ChatGPT subscription access token renewed");
        } else {
            // The new token still serves this request; the next renewal
            // starts from the stored refresh token.
            tracing::error!("The renewed ChatGPT subscription sign-in could not be stored");
        }
        Ok(Access { token, account_id })
    }
}

/// What a plan request fetches per request ([`ProviderCredentials`]).
pub struct PlanCredentials {
    pub(super) app: Weak<AppState>,
    pub(super) sign_in: Arc<SignIn>,
}

#[async_trait]
impl ProviderCredentials for PlanCredentials {
    async fn fetch(&self) -> Result<ProviderAuth, ModelError> {
        let Some(app) = self.app.upgrade() else {
            return Err(ModelError::MissingCredential(SIGN_IN_AGAIN.into()));
        };
        let access = self.sign_in.access(&app).await?;
        Ok(ProviderAuth {
            key: Some(access.token),
            headers: super::codex::headers(access.account_id.as_deref()),
        })
    }
}
