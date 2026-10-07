//! Broker sign-in, session persistence, resume and revocation.
//!
//! * OAuth brokers: `start_oauth` stores a server-generated `state` (with
//!   the redirect address the authorize URL was built with) and returns the
//!   broker's authorize URL; a broker whose URL needs a call first (Dhan's
//!   consent) builds it in `Broker::begin_login`. The callback
//!   (`complete_oauth`) consumes the state (Dhan, whose redirect carries no
//!   state: the newest pending Dhan sign-in, single use, 10 minutes),
//!   exchanges the code for a token in Rust, persists it encrypted and
//!   publishes `broker.connected`. The code never reaches the frontend.
//! * Form logins: `login_with_form`, with the fields
//!   `catalog::login_fields` declares for the broker (Groww's pasted token,
//!   Kotak's mobile, TOTP and MPIN, Dhan's pasted access token), else the
//!   web's client id / PIN / TOTP names.
//! * Every successful sign-in or resume starts the session's streaming
//!   (`BrokerRuntime::activate`: master contract, feeds, order updates);
//!   `revoke` tears it down.
//! * After a password sign-in, `try_resume` brings back the stored session
//!   if it was issued after the last 03:00 IST boundary and the broker still
//!   accepts it (validated with a funds call, like the web).
//! * Logout and the daily boundary call `revoke`.

use crate::brokers::{catalog, BrokerCredentials};
use crate::db::sqlite::{auth, credentials, oauth_state};
use crate::error::{AppError, Result};
use crate::events::{Event, SessionEndReason};
use crate::security::Secret;
use crate::session::web::random_token;
use crate::state::{AppState, BrokerSession};
use std::collections::HashMap;
use std::time::Duration;

pub const OAUTH_STATE_TTL_MINUTES: i64 = 10;
const RESUME_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

pub struct BrokerAuthService;

/// Where a broker callback came from.
#[derive(Debug, Clone, Copy)]
pub enum CallbackOrigin<'a> {
    /// The broker redirected the browser here; `session_id` is the browser
    /// session's cookie, when it was sent.
    Redirect { session_id: Option<&'a str> },
    /// The signed-in trader pasted the address into OpenAlgo.
    Manual { session_id: &'a str },
}

/// Inputs from a broker login form (web field names).
#[derive(Debug, Default, Clone)]
pub struct FormLogin {
    pub client_id: Option<String>,
    pub password: Option<Secret>,
    pub totp: Option<Secret>,
}

impl FormLogin {
    /// The broker's own fields (`catalog::login_fields`) when it declares
    /// them, with required ones checked; otherwise the web's common names.
    pub fn for_broker(broker: &str, f: &HashMap<String, String>) -> Result<Self> {
        let fields = catalog::login_fields(broker);
        if fields.is_empty() {
            return Ok(Self::from_fields(f));
        }
        let mut out = FormLogin::default();
        for lf in fields {
            let v = f
                .get(lf.name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            let Some(v) = v else {
                if lf.required {
                    return Err(AppError::Validation(format!(
                        "Enter the {} to sign in.",
                        lf.label.to_lowercase()
                    )));
                }
                continue;
            };
            match catalog::credential_slot(lf.name) {
                Some(catalog::CredentialSlot::ClientId) => out.client_id = Some(v),
                Some(catalog::CredentialSlot::Password) => out.password = Some(Secret::new(v)),
                Some(catalog::CredentialSlot::Totp) => out.totp = Some(Secret::new(v)),
                None => tracing::warn!("Login field {} has no credential slot", lf.name),
            }
        }
        Ok(out)
    }

    pub fn from_fields(f: &HashMap<String, String>) -> Self {
        let pick = |keys: &[&str]| {
            keys.iter().find_map(|k| {
                f.get(*k)
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| v.trim().to_string())
            })
        };
        FormLogin {
            client_id: pick(&["userid", "clientid", "client_id", "mobile"]),
            password: pick(&["pin", "password", "mpin"]).map(Secret::new),
            totp: pick(&["totp", "twofa", "otp"]).map(Secret::new),
        }
    }
}

impl BrokerAuthService {
    /// Broker chosen in settings (web: from REDIRECT_URL).
    pub fn active_broker(state: &AppState) -> Option<String> {
        state.server_config().active_broker
    }

    fn load_credentials(
        state: &AppState,
        broker: &str,
    ) -> Result<credentials::BrokerCredentialSet> {
        let conn = state.sqlite.conn()?;
        credentials::load(&conn, &state.security, broker)?
            .filter(|c| !c.api_key.is_empty())
            .ok_or_else(|| {
                AppError::Validation(
                    "Add your broker API key and secret in Profile, Broker Configuration, then try again."
                        .into(),
                )
            })
    }

    /// The stored broker credentials as adapter input (no login secrets).
    fn stored_input(creds: &credentials::BrokerCredentialSet) -> BrokerCredentials {
        BrokerCredentials {
            api_key: creds.api_key.expose().to_string(),
            api_secret: creds.api_secret.as_ref().map(|s| s.expose().to_string()),
            client_id: creds.client_id.clone(),
            api_key_market: creds
                .api_key_market
                .as_ref()
                .map(|s| s.expose().to_string()),
            api_secret_market: creds
                .api_secret_market
                .as_ref()
                .map(|s| s.expose().to_string()),
            ..Default::default()
        }
    }

    /// Start an OAuth login: store a fresh `state` with the redirect
    /// address, return the authorize URL.
    pub async fn start_oauth(
        state: &AppState,
        broker: &str,
        session_id: Option<&str>,
    ) -> Result<String> {
        let Some(adapter) = state.brokers.get(broker) else {
            return Err(AppError::Validation(format!(
                "Signing in to {} is not available in this version of OpenAlgo Desktop yet.",
                broker
            )));
        };
        let creds = Self::load_credentials(state, broker)?;
        let redirect = state.server_config().redirect_url_for(broker);
        let st = random_token();
        let url = match catalog::authorize_url(broker, creds.api_key.expose(), &redirect, &st) {
            Some(a) => a.url,
            None => {
                let input = BrokerCredentials {
                    redirect_uri: Some(redirect.clone()),
                    ..Self::stored_input(&creds)
                };
                // No database connection is held across this await.
                adapter.begin_login(&input).await?.ok_or_else(|| {
                    AppError::Validation(format!(
                        "{} signs in with a form, not a browser redirect.",
                        broker
                    ))
                })?
            }
        };
        {
            let conn = state.sqlite.conn()?;
            oauth_state::insert(
                &conn,
                &st,
                broker,
                Some(&redirect),
                session_id,
                state.now(),
                chrono::Duration::minutes(OAUTH_STATE_TTL_MINUTES),
            )?;
        }
        Ok(url)
    }

    /// Finish an OAuth login from the callback query parameters.
    pub async fn complete_oauth(
        state: &AppState,
        broker: &str,
        params: &HashMap<String, String>,
        origin: CallbackOrigin<'_>,
    ) -> Result<BrokerSession> {
        let st = params.get("state").cloned().unwrap_or_default();
        let session_id = match origin {
            CallbackOrigin::Redirect { session_id } => session_id,
            CallbackOrigin::Manual { session_id } => Some(session_id),
        };
        let pending = if !st.is_empty() {
            let conn = state.sqlite.conn()?;
            oauth_state::consume(&conn, &st, broker, state.now())?
        } else if let (false, Some(sid)) = (catalog::callback_carries_state(broker), session_id) {
            let conn = state.sqlite.conn()?;
            oauth_state::consume_latest(&conn, broker, sid, state.now())?
        } else {
            None
        };
        let pasted = catalog::pasted_token(broker, params);
        // A token the signed-in trader pasted needs no pending sign-in; one
        // arriving by redirect does, like any code.
        let pending = match (pending, &pasted, origin) {
            (Some(p), _, _) => Some(p),
            (None, Some(_), CallbackOrigin::Manual { .. }) => {
                Some(oauth_state::Pending { redirect_uri: None })
            }
            _ => None,
        };
        let Some(pending) = pending else {
            return Err(AppError::Auth(
                "This broker sign-in was not started from OpenAlgo or has expired. Start the broker login again from OpenAlgo."
                    .into(),
            ));
        };
        let creds = Self::load_credentials(state, broker)?;
        if let Some((token, uid)) = pasted {
            let stored = Self::stored_input(&creds);
            let input = BrokerCredentials {
                password: Some(token),
                client_id: uid.or(stored.client_id.clone()),
                ..stored
            };
            return Self::authenticate(state, broker, input).await;
        }
        let code = catalog::extract_code(broker, params).ok_or_else(|| {
            AppError::Auth("The broker did not complete the sign-in. Try again.".into())
        })?;
        let input = BrokerCredentials {
            request_token: Some(code.clone()),
            auth_code: Some(code),
            redirect_uri: pending.redirect_uri,
            ..Self::stored_input(&creds)
        };
        Self::authenticate(state, broker, input).await
    }

    /// Form login (Angel and the other client-id/PIN/TOTP brokers).
    pub async fn login_with_form(
        state: &AppState,
        broker: &str,
        form: FormLogin,
    ) -> Result<BrokerSession> {
        if catalog::auth_type(broker) != catalog::AuthType::Form
            && catalog::login_fields(broker).is_empty()
        {
            return Err(AppError::Validation(
                "This broker signs in through the broker's own page.".into(),
            ));
        }
        let creds = Self::load_credentials(state, broker)?;
        let stored = Self::stored_input(&creds);
        let input = BrokerCredentials {
            client_id: form.client_id.or(stored.client_id.clone()),
            password: form.password.map(|s| s.expose().to_string()),
            totp: form.totp.map(|s| s.expose().to_string()),
            ..stored
        };
        Self::authenticate(state, broker, input).await
    }

    async fn authenticate(
        state: &AppState,
        broker_id: &str,
        input: BrokerCredentials,
    ) -> Result<BrokerSession> {
        let broker = state.brokers.get(broker_id).ok_or_else(|| {
            AppError::Validation(format!(
                "Signing in to {} is not available in this version of OpenAlgo Desktop yet.",
                broker_id
            ))
        })?;
        // No database connection is held across this await.
        let resp = broker.authenticate(input).await?;
        let session = BrokerSession {
            broker_id: broker_id.to_string(),
            auth_token: Secret::new(resp.auth_token),
            feed_token: resp.feed_token.map(Secret::new),
            user_id: resp.user_id,
            user_name: resp.user_name,
            authenticated_at: state.now(),
        };
        Self::persist(state, &session)?;
        Self::activate(state, &session).await;
        Ok(session)
    }

    /// Start the session's streaming (master contract, feeds, order
    /// updates).
    async fn activate(state: &AppState, session: &BrokerSession) {
        match state.arc() {
            Some(ctx) => ctx.runtime.activate(&ctx, session).await,
            None => tracing::error!("Broker streaming could not start: the app is shutting down"),
        }
    }

    /// Store and activate a session, then announce it.
    pub fn persist(state: &AppState, s: &BrokerSession) -> Result<()> {
        {
            let conn = state.sqlite.conn()?;
            auth::upsert(
                &conn,
                &state.security,
                &auth::StoredBrokerSession {
                    broker_id: s.broker_id.clone(),
                    auth_token: s.auth_token.clone(),
                    feed_token: s.feed_token.clone(),
                    user_id: Some(s.user_id.clone()),
                    user_name: s.user_name.clone(),
                    authenticated_at: s.authenticated_at,
                },
            )?;
            crate::config::save(
                &conn,
                &crate::config::ServerConfigUpdate {
                    active_broker: Some(s.broker_id.clone()),
                    ..Default::default()
                },
            )?;
        }
        let _ = state.reload_config();
        state.set_broker_session(Some(s.clone()));
        state.api_keys.clear();
        state.bus.publish(Event::BrokerConnected {
            broker: s.broker_id.clone(),
            user_id: Some(s.user_id.clone()),
        });
        tracing::info!("Broker session started for {}", s.broker_id);
        Ok(())
    }

    /// Resume the stored session after a password sign-in. `Ok(None)` when
    /// there is nothing to resume or the broker no longer accepts it.
    pub async fn try_resume(state: &AppState) -> Result<Option<BrokerSession>> {
        if let Some(s) = state.get_broker_session() {
            return Ok(Some(s));
        }
        let stored = {
            let conn = state.sqlite.conn()?;
            match auth::latest_active(&conn, &state.security) {
                Ok(s) => s,
                Err(AppError::Locked) => None,
                Err(e) => return Err(e),
            }
        };
        let Some(stored) = stored else {
            return Ok(None);
        };
        let cfg = state.server_config();
        if !crate::session::boundary::is_fresh(
            stored.authenticated_at,
            state.now(),
            cfg.session_expiry_hour,
            cfg.session_expiry_minute,
        ) {
            let conn = state.sqlite.conn()?;
            auth::revoke(&conn, &stored.broker_id)?;
            return Ok(None);
        }
        let Some(broker) = state.brokers.get(&stored.broker_id) else {
            return Ok(None);
        };
        // Per-login state the token does not carry (Kotak's UCC) comes back
        // from the stored broker credentials.
        let stored_creds = {
            let conn = state.sqlite.conn()?;
            credentials::load(&conn, &state.security, &stored.broker_id)?
        };
        if let Some(c) = &stored_creds {
            broker.restore_session(&Self::stored_input(c));
        }
        // Like the web: a cheap funds call proves the token still works.
        match tokio::time::timeout(
            RESUME_CHECK_TIMEOUT,
            broker.get_funds(&crate::brokers::types::AuthToken::new(
                stored.auth_token.expose(),
            )),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::info!("Stored broker session not accepted by broker: {}", e.code());
                return Ok(None);
            }
            Err(_) => {
                tracing::info!("Broker did not answer the session check in time");
                return Ok(None);
            }
        }
        let session = BrokerSession {
            broker_id: stored.broker_id,
            auth_token: stored.auth_token,
            feed_token: stored.feed_token,
            user_id: stored.user_id.unwrap_or_default(),
            user_name: stored.user_name,
            authenticated_at: stored.authenticated_at,
        };
        state.set_broker_session(Some(session.clone()));
        state.bus.publish(Event::BrokerConnected {
            broker: session.broker_id.clone(),
            user_id: Some(session.user_id.clone()),
        });
        tracing::info!("Resumed broker session for {}", session.broker_id);
        Self::activate(state, &session).await;
        Ok(Some(session))
    }

    /// End the broker session everywhere: stored row revoked, memory cleared,
    /// streaming torn down (feeds closed, subscriptions cleared, owned tasks
    /// aborted, adapter pollers stopped), symbol cache dropped, subscribers
    /// told.
    pub async fn revoke(state: &AppState, reason: SessionEndReason) -> Result<()> {
        {
            let conn = state.sqlite.conn()?;
            auth::revoke_all(&conn)?;
        }
        state.set_broker_session(None);
        state.api_keys.clear();
        state.runtime.teardown(state).await;
        state.clear_symbol_cache();
        state.bus.publish(Event::BrokerSessionEnded { reason });
        Ok(())
    }

    /// Whether the stored broker session ended (revoked/expired) rather than
    /// never having existed.
    pub fn had_revoked_session(state: &AppState) -> bool {
        state
            .sqlite
            .conn()
            .ok()
            .and_then(|c| auth::has_revoked(&c).ok())
            .unwrap_or(false)
    }
}
