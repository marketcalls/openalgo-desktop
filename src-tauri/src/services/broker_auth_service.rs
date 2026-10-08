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
    /// Date of birth (Motilal's second factor, `DD/MM/YYYY`).
    pub dob: Option<Secret>,
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
                Some(catalog::CredentialSlot::Dob) => out.dob = Some(Secret::new(v)),
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
            dob: None,
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
        let binding = catalog::login_binding(broker, &url);
        {
            let conn = state.sqlite.conn()?;
            oauth_state::insert(
                &conn,
                &st,
                broker,
                Some(&redirect),
                session_id,
                binding.as_deref(),
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
        let now = state.now();
        let mut state_less = false;
        // Every path needs a pending sign-in this OpenAlgo started for this
        // broker, used once.
        let pending = {
            let conn = state.sqlite.conn()?;
            match origin {
                // The broker's redirect (GET, or the XTS form POST): by
                // `state`; without it only for brokers that drop it, from
                // the same browser session, within three minutes.
                CallbackOrigin::Redirect { session_id } => {
                    if !st.is_empty() {
                        oauth_state::consume(&conn, &st, broker, now)?
                    } else if let (false, Some(sid)) =
                        (catalog::callback_carries_state(broker), session_id)
                    {
                        state_less = true;
                        oauth_state::consume_latest(
                            &conn,
                            broker,
                            sid,
                            now,
                            chrono::Duration::seconds(oauth_state::STATELESS_WINDOW_SECONDS),
                        )?
                    } else {
                        None
                    }
                }
                // Pasted by the signed-in trader (CSRF-checked route): a
                // sign-in this same session started, by `state` when the
                // address has one, else its newest one for this broker.
                CallbackOrigin::Manual { session_id } => {
                    if !st.is_empty() {
                        oauth_state::consume_for_session(&conn, &st, broker, session_id, now)?
                    } else {
                        oauth_state::consume_latest(
                            &conn,
                            broker,
                            session_id,
                            now,
                            chrono::Duration::minutes(OAUTH_STATE_TTL_MINUTES),
                        )?
                    }
                }
            }
        };
        // A ready token is accepted only when the signed-in trader pasted
        // the address into OpenAlgo; one arriving by redirect is ignored,
        // since nothing ties it to this trader.
        let pasted = match origin {
            CallbackOrigin::Manual { .. } => catalog::pasted_token(broker, params),
            CallbackOrigin::Redirect { .. } => None,
        };
        let Some(pending) = pending else {
            return Err(AppError::Auth(
                "This broker sign-in was not started from OpenAlgo or has expired. Start the broker login again from OpenAlgo."
                    .into(),
            ));
        };
        // A login id the callback repeats must be the one this sign-in
        // started with (Dhan's consent).
        if let Some(id) = catalog::callback_binding(broker, params) {
            if !pending.binding_matches(&id) {
                tracing::warn!(broker, "Broker sign-in refused: the consent does not match");
                return Err(AppError::Auth(
                    "This broker sign-in does not match the one started from OpenAlgo. Start the broker login again from OpenAlgo."
                        .into(),
                ));
            }
        }
        let creds = Self::load_credentials(state, broker)?;
        let expected = Self::expected_account(state, broker, &creds, state_less)?;
        if let Some((token, uid)) = pasted {
            let stored = Self::stored_input(&creds);
            let input = BrokerCredentials {
                password: Some(token),
                client_id: uid.or(stored.client_id.clone()),
                ..stored
            };
            return Self::authenticate_as(state, broker, input, expected.as_deref()).await;
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
        Self::authenticate_as(state, broker, input, expected.as_deref()).await
    }

    /// The broker account a sign-in must belong to, on every path: the
    /// account the stored credentials name (Dhan, the Noren family), else
    /// the account of the last session with this broker (forgotten when the
    /// trader saves that broker's settings again, to switch accounts).
    /// `None` on a first sign-in, except for a callback without `state`,
    /// which is refused then: the browser session alone does not prove the
    /// trader started it.
    fn expected_account(
        state: &AppState,
        broker: &str,
        creds: &credentials::BrokerCredentialSet,
        state_less: bool,
    ) -> Result<Option<String>> {
        if let Some(a) =
            catalog::configured_account(broker, creds.api_key.expose(), creds.client_id.as_deref())
        {
            return Ok(Some(a));
        }
        let last = {
            let conn = state.sqlite.conn()?;
            auth::last_user_id(&conn, broker)?
        };
        match last {
            Some(u) => Ok(Some(u)),
            None if !state_less => Ok(None),
            None if catalog::CLIENT_ID_BROKERS.contains(&broker) => {
                Err(AppError::Validation(format!(
                    "Add your {} client id in Profile, Broker Configuration, then start the broker login again.",
                    broker
                )))
            }
            None => Err(AppError::Validation(format!(
                "Enter your {} API key as client_id:::api_key in Profile, Broker Configuration, then start the broker login again.",
                broker
            ))),
        }
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
        let expected = Self::expected_account(state, broker, &creds, false)?;
        let stored = Self::stored_input(&creds);
        let input = BrokerCredentials {
            client_id: form.client_id.or(stored.client_id.clone()),
            password: form.password.map(|s| s.expose().to_string()),
            totp: form.totp.map(|s| s.expose().to_string()),
            // A form login has no OAuth code; the one extra factor (Motilal's
            // date of birth, `CredentialSlot::Dob`) travels in its place.
            auth_code: form.dob.map(|s| s.expose().to_string()),
            ..stored
        };
        Self::authenticate_as(state, broker, input, expected.as_deref()).await
    }

    /// Prepare a form login when its page opens or the trader asks again:
    /// Definedge sends the login OTP (web GET `/definedge/callback` and the
    /// `resend` action). `Ok(None)` for brokers with nothing to prepare,
    /// otherwise the broker's message for the trader.
    pub async fn prepare_form_login(state: &AppState, broker: &str) -> Result<Option<String>> {
        let Some(adapter) = state.brokers.get(broker) else {
            return Ok(None);
        };
        let Some(definedge) = adapter
            .as_any()
            .and_then(|a| a.downcast_ref::<crate::brokers::definedge::DefinedgeBroker>())
        else {
            return Ok(None);
        };
        let input = Self::stored_input(&Self::load_credentials(state, broker)?);
        // No database connection is held across this await.
        definedge.send_otp(&input).await.map(Some)
    }

    /// Samco static IP check (web GET `/samco/ip-status`):
    /// `(http status, body)` with the web's success and error shapes.
    pub async fn samco_ip_status(state: &AppState) -> (u16, serde_json::Value) {
        use crate::brokers::samco::{self, SamcoBroker};
        let session = state
            .get_broker_session()
            .filter(|s| s.broker_id == "samco");
        let adapter = state.brokers.get("samco");
        let samco = adapter
            .as_ref()
            .and_then(|a| a.as_any())
            .and_then(|a| a.downcast_ref::<SamcoBroker>());
        let (Some(session), Some(samco)) = (session, samco) else {
            return samco::ip_status_error(samco::IP_STATUS_NOT_CONNECTED);
        };
        let auth = crate::brokers::types::AuthToken::new(session.auth_token.expose());
        match samco.ip_status(&auth).await {
            Ok(s) => (200, s.to_json()),
            Err(e) => {
                tracing::warn!("Samco IP status check failed: {}", e.code());
                samco::ip_status_error(&e.client_message())
            }
        }
    }

    /// Sign in; when `expected` names an account, a session for any other
    /// account is refused before anything is stored or replaced.
    async fn authenticate_as(
        state: &AppState,
        broker_id: &str,
        input: BrokerCredentials,
        expected: Option<&str>,
    ) -> Result<BrokerSession> {
        let broker = state.brokers.get(broker_id).ok_or_else(|| {
            AppError::Validation(format!(
                "Signing in to {} is not available in this version of OpenAlgo Desktop yet.",
                broker_id
            ))
        })?;
        // No database connection is held across this await.
        let resp = broker.authenticate(input).await?;
        if let Some(want) = expected {
            if !resp.user_id.trim().eq_ignore_ascii_case(want.trim()) {
                tracing::warn!(
                    broker = broker_id,
                    "Broker sign-in refused: it belongs to a different account than the one configured"
                );
                return Err(AppError::Auth(format!(
                    "This sign-in is for a different {} account than the one set up in OpenAlgo. Nothing was changed. Log in with your own account; to switch accounts, log out and save the broker settings again first.",
                    broker_id
                )));
            }
        }
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

#[cfg(test)]
mod form_tests {
    use super::*;

    #[test]
    fn form_carries_the_date_of_birth() {
        let f: HashMap<String, String> = [
            ("userid", "AB1"),
            ("password", "pw"),
            ("dob", " 18/10/1988 "),
            ("totp", "123456"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        // Through Motilal's declared fields (`catalog::login_fields`).
        let form = FormLogin::for_broker("motilal", &f).unwrap();
        assert_eq!(form.client_id.as_deref(), Some("AB1"));
        assert_eq!(form.password.as_ref().map(|s| s.expose()), Some("pw"));
        assert_eq!(form.dob.as_ref().map(|s| s.expose()), Some("18/10/1988"));
        assert_eq!(form.totp.as_ref().map(|s| s.expose()), Some("123456"));
        // The date of birth is required; the TOTP is not.
        let mut no_dob = f.clone();
        no_dob.remove("dob");
        assert!(FormLogin::for_broker("motilal", &no_dob).is_err());
        let mut no_totp = f.clone();
        no_totp.remove("totp");
        assert!(FormLogin::for_broker("motilal", &no_totp)
            .unwrap()
            .totp
            .is_none());
        // Brokers without the field never pick it up.
        assert!(FormLogin::from_fields(&f).dob.is_none());
    }
}
