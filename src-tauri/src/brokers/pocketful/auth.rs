//! Pocketful sign-in: the OAuth 2.0 code exchange and the trading account
//! behind it (web `api/auth_api.py`, marketcalls/openalgo#2467).
//!
//! * The authorize address (`catalog::authorize_url`) is
//!   `https://algo.pocketful.in/oauth2/auth` with `client_id`,
//!   `redirect_uri`, `response_type=code`, `scope=orders holdings` and the
//!   server-issued `state`, as Pocketful's developer portal opens it.
//! * The code exchange (`POST /oauth2/token` on the same host, RFC 6749
//!   4.1.3) uses HTTP Basic with the App ID and App Secret and repeats the
//!   redirect URI the sign-in started with, which the pending sign-in
//!   records (`BrokerCredentials::redirect_uri`).
//! * The trading account comes from `GET /api/v1/user/trading_info`
//!   (`data.client_id`); an answer outside the shape of a client id is
//!   refused, so no session is ever stored without its account.
//!
//! Every failure is a sentence for the trader; the broker's own words go to
//! the log, never the code, the App Secret or the access token.

use super::client::{Answer, Lane, Request};
use super::PocketfulBroker;
use crate::brokers::common::redact::url_safe;
use crate::brokers::types::AuthToken;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use base64::Engine;
use reqwest::StatusCode;
use serde_json::Value;
use std::collections::HashMap;

/// The web default (`REDIRECT_URL` unset) for the shipped port.
pub const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:5000/pocketful/callback";
/// The scope Pocketful's own portal asks for (web `SCOPE`).
pub const SCOPE: &str = "orders holdings";
const TRADING_INFO_PATH: &str = "/api/v1/user/trading_info";

pub const MISSING_CODE: &str = "Pocketful did not send a login code back to OpenAlgo. Start the Pocketful login again from Select Broker.";
pub const MISSING_KEYS: &str = "The App ID or App Secret for Pocketful is missing. Add both on Broker Configuration, then log in again.";
pub const UNREACHABLE: &str = "OpenAlgo could not reach Pocketful to finish the login. Check your internet connection, then start the Pocketful login again.";
pub const KEYS_REFUSED: &str = "Pocketful did not accept the App ID or App Secret. Check both against your app on the Pocketful API portal, update them on Broker Configuration, then log in again.";
pub const REDIRECT_MISMATCH: &str = "Pocketful refused the login because the redirect URL does not match your Pocketful app. Register the redirect URL shown on Broker Configuration as your app's custom redirect URL on the Pocketful API portal, then log in again.";
pub const CODE_REFUSED: &str =
    "The Pocketful login code has expired or was already used. Start the Pocketful login again.";
pub const BROKER_UNAVAILABLE: &str =
    "Pocketful could not finish the login just now. Wait a minute, then start the Pocketful login again.";
pub const LOGIN_REFUSED: &str = "Pocketful did not complete the login. Start the Pocketful login again. If it fails again, check your app on the Pocketful API portal.";
pub const NO_SESSION: &str =
    "Pocketful accepted the login but sent no session back. Start the Pocketful login again.";
pub const NO_ACCOUNT: &str = "Pocketful signed you in but did not say which trading account the login is for, so OpenAlgo cannot use it. Start the Pocketful login again.";
pub const LOGIN_CANCELLED: &str = "The Pocketful login was cancelled or not approved. Start the Pocketful login again and approve access.";
pub const SCOPE_REFUSED: &str = "Pocketful did not allow the access OpenAlgo asks for at login, so the login stopped. Your keys are not the cause; report this to the OpenAlgo team.";

/// The redirect URI for the code exchange: the one the authorize URL was
/// built with, else the web default.
pub fn redirect_uri(creds: &BrokerCredentials) -> String {
    creds
        .redirect_uri
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .unwrap_or(DEFAULT_REDIRECT_URI)
        .to_string()
}

/// The sign-in address, as Pocketful's developer portal opens it; the code
/// exchange repeats `redirect_url` exactly.
pub fn authorize_url(app_id: &str, redirect_url: &str, state: &str) -> String {
    let enc = |s: &str| urlencoding::encode(s).into_owned();
    format!(
        "{}/oauth2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
        super::BASE_URL,
        enc(app_id),
        enc(redirect_url),
        enc(SCOPE),
        enc(state)
    )
}

/// A broker-supplied value as it may go into one log line: at most
/// `limit` characters, any URL without its query.
fn short(value: &str, limit: usize) -> String {
    url_safe(&value.chars().take(limit).collect::<String>())
}

/// The sentence for a callback Pocketful sent back without a usable code
/// (web `callback_refusal` and `MISSING_CODE`): `error` per RFC 6749
/// 4.1.2.1 (`access_denied` when the trader declined), else a blank code.
/// `None` when the callback carries a code.
pub fn callback_refusal(params: &HashMap<String, String>) -> Option<&'static str> {
    if let Some(error) = params.get("error").filter(|e| !e.trim().is_empty()) {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("");
        tracing::warn!(
            "Pocketful sign-in came back without a code: error={:?} description={:?}",
            short(error, 64),
            short(description, 300)
        );
        return Some(match error.trim() {
            "access_denied" => LOGIN_CANCELLED,
            "invalid_scope" => SCOPE_REFUSED,
            _ => LOGIN_REFUSED,
        });
    }
    if params.get("code").is_none_or(|c| c.trim().is_empty()) {
        tracing::warn!("Pocketful callback arrived without a login code");
        return Some(MISSING_CODE);
    }
    None
}

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").to_string()
}

/// The sentence for a token endpoint refusal (RFC 6749 5.2). Every
/// `invalid_grant` description mentions the "redirection URI"; only the
/// hint's own `redirect_uri` names a mismatch.
fn token_refusal(status: StatusCode, body: Option<&Value>) -> &'static str {
    let error = short(&text(body.and_then(|b| b.get("error"))), 64);
    let detail = format!(
        "{} {}",
        short(&text(body.and_then(|b| b.get("error_description"))), 300),
        short(&text(body.and_then(|b| b.get("error_hint"))), 300)
    );
    tracing::warn!(
        status = status.as_u16(),
        "Pocketful refused the login code exchange: error={:?} detail={:?}",
        error,
        detail.trim()
    );
    if error == "invalid_client" || status == StatusCode::UNAUTHORIZED {
        KEYS_REFUSED
    } else if detail.to_ascii_lowercase().contains("redirect_uri") {
        REDIRECT_MISMATCH
    } else if error == "invalid_grant" {
        CODE_REFUSED
    } else if status.is_server_error() {
        BROKER_UNAVAILABLE
    } else {
        LOGIN_REFUSED
    }
}

/// `POST /oauth2/token`: the access token, or the trader's sentence.
async fn exchange_code(
    b: &PocketfulBroker,
    code: &str,
    app_id: &str,
    app_secret: &str,
    redirect: &str,
) -> Result<String> {
    let basic =
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", app_id, app_secret));
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect),
    ];
    let sent = b
        .http
        .post(format!("{}/oauth2/token", b.urls.rest))
        .timeout(super::client::REQUEST_TIMEOUT)
        .header("Authorization", format!("Basic {}", basic))
        .header("Accept", "application/json")
        .header("Cache-Control", "no-cache")
        .form(&form)
        .send()
        .await;
    let resp = match sent {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "Pocketful login code exchange did not reach Pocketful: {}",
                super::redact(e.into())
            );
            return Err(AppError::Auth(UNREACHABLE.into()));
        }
    };
    let status = resp.status();
    let body = match resp.bytes().await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
        Err(e) => {
            tracing::warn!(
                "Pocketful's answer to the login code exchange was lost: {}",
                super::redact(e.into())
            );
            return Err(AppError::Auth(UNREACHABLE.into()));
        }
    };
    if !status.is_success() {
        return Err(AppError::Auth(token_refusal(status, body.as_ref()).into()));
    }
    let access = text(body.as_ref().and_then(|v| v.get("access_token")));
    if access.trim().is_empty() {
        let fields: Vec<String> = body
            .as_ref()
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        tracing::warn!(
            status = status.as_u16(),
            "Pocketful answered the login code exchange without an access token (fields {:?})",
            fields
        );
        return Err(AppError::Auth(NO_SESSION.into()));
    }
    Ok(access.trim().to_string())
}

/// A trading client id as the docs and SDKs show them (DEMO1, YA0002): it
/// goes into request paths, so an answer outside this shape is refused
/// rather than stored (web `_CLIENT_ID`).
pub(crate) fn is_client_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// The trading account Pocketful reports for `access` (web
/// `_trading_client_id`).
async fn trading_client_id(b: &PocketfulBroker, access: &str) -> Result<String> {
    let auth = AuthToken::new(access);
    let answer: Answer = match b
        .send(Lane::Data, &auth, Request::get(TRADING_INFO_PATH))
        .await
    {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(
                "Pocketful account lookup after login did not reach Pocketful: {}",
                e.code()
            );
            return Err(AppError::Auth(UNREACHABLE.into()));
        }
    };
    let id = if answer.ok() {
        text(answer.payload().and_then(|d| d.get("client_id")))
            .trim()
            .to_string()
    } else {
        String::new()
    };
    if !is_client_id(&id) {
        tracing::warn!(
            status = answer.status.as_u16(),
            "Pocketful did not report a usable trading account after login: message={:?} account present={}",
            short(&answer.message(), 300),
            !id.is_empty()
        );
        return Err(AppError::Auth(NO_ACCOUNT.into()));
    }
    Ok(id)
}

/// Exchange Pocketful's login code for a session and find its trading
/// account (web `authenticate_broker`). The callback's `state` was matched
/// to a sign-in started here and used up before this is called.
pub async fn authenticate(b: &PocketfulBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let redirect = redirect_uri(&creds);
    let code = creds
        .auth_code
        .as_deref()
        .or(creds.request_token.as_deref())
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| AppError::Validation(MISSING_CODE.into()))?;
    let app_id = creds.api_key.trim();
    let app_secret = creds.api_secret.as_deref().map(str::trim).unwrap_or("");
    if app_id.is_empty() || app_secret.is_empty() {
        return Err(AppError::Validation(MISSING_KEYS.into()));
    }
    let access = exchange_code(b, code, app_id, app_secret, &redirect).await?;
    let client_id = trading_client_id(b, &access).await?;
    tracing::info!("Pocketful login exchanged its code and found the trading account");
    Ok(AuthResponse {
        auth_token: access,
        // The feed signs in with the access token and the client id.
        feed_token: None,
        user_name: None,
        user_id: client_id,
    })
}
