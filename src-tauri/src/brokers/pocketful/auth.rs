//! Pocketful OAuth2 code exchange (web `api/auth_api.py`).
//!
//! `POST /oauth2/token` with `Authorization: Basic base64(client_id:secret)`
//! and the form `grant_type=authorization_code&code=..&redirect_uri=..`,
//! then `GET /api/v1/user/trading_info` (Bearer) for `data.client_id`,
//! which the web stores as the session's user id. The exchange must repeat
//! the redirect URI of the authorize URL, which the pending sign-in records
//! and hands over as `BrokerCredentials::redirect_uri`.

use super::{mapping, PocketfulBroker};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use base64::Engine;
use serde_json::Value;

/// The web default (`REDIRECT_URL` unset) for the shipped port.
pub const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:5000/pocketful/callback";

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

/// `Basic base64(client_id:client_secret)`.
pub fn basic_auth(client_id: &str, client_secret: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", client_id, client_secret))
    )
}

/// The token form `authenticate_broker` posts.
pub fn token_form(code: &str, redirect: &str) -> Vec<(&'static str, String)> {
    vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect.to_string()),
    ]
}

pub async fn authenticate(b: &PocketfulBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let redirect = redirect_uri(&creds);
    let code = creds
        .auth_code
        .or(creds.request_token)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Pocketful did not return a login code. Start the Pocketful login again.".into(),
            )
        })?;
    let client_id = creds.api_key.trim().to_string();
    let secret = creds
        .api_secret
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let (Some(secret), false) = (secret, client_id.is_empty()) else {
        return Err(AppError::Validation(
            "Your Pocketful client id or client secret is missing. Add them on the broker settings page."
                .into(),
        ));
    };
    let resp = b
        .http
        .post(format!("{}/oauth2/token", b.urls.rest))
        .header("Authorization", basic_auth(&client_id, &secret))
        .header("Cache-Control", "no-cache")
        .form(&token_form(code.trim(), &redirect))
        .send()
        .await
        .map_err(|e| super::redact(e.into()))?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| super::redact(e.into()))?;
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !status.is_success() {
        let detail = mapping::text(&body, "message");
        let error = mapping::text(&body, "error");
        tracing::warn!(
            status = status.as_u16(),
            error = %error,
            "Pocketful token exchange refused: {}",
            detail
        );
        let lower = format!("{} {}", detail, error).to_ascii_lowercase();
        return Err(AppError::Auth(if lower.contains("redirect") {
            "Pocketful refused the login because the redirect URL does not match your Pocketful app. Set the app's redirect URL to the one shown in OpenAlgo, then log in again."
                .into()
        } else if lower.contains("client") || status.as_u16() == 401 {
            "Pocketful did not accept your client id or secret. Check them on the broker settings page."
                .into()
        } else {
            "The Pocketful login code has expired or was already used. Log in to Pocketful again."
                .into()
        }));
    }
    let access_token = mapping::text(&body, "access_token");
    if access_token.is_empty() {
        return Err(AppError::Auth(
            "Pocketful accepted the login but returned no session. Log in to Pocketful again."
                .into(),
        ));
    }
    // web: then fetch the trading client id with the new token.
    let resp = b
        .http
        .get(format!("{}/api/v1/user/trading_info", b.urls.rest))
        .bearer_auth(&access_token)
        .send()
        .await
        .map_err(|e| super::redact(e.into()))?;
    let (status, info): (_, Value) = http::read_json("pocketful", resp)
        .await
        .map_err(super::redact)?;
    let user_id = mapping::text(&info["data"], "client_id");
    if !status.is_success()
        || info.get("status").and_then(Value::as_str) != Some("success")
        || user_id.is_empty()
    {
        tracing::warn!(
            status = status.as_u16(),
            "Pocketful trading_info failed after login: {}",
            mapping::text(&info, "message")
        );
        return Err(AppError::Auth(
            "Pocketful signed you in but did not return your trading account id. Log in to Pocketful again."
                .into(),
        ));
    }
    Ok(AuthResponse {
        auth_token: access_token,
        // web returns feed_token None; the feed uses the access token.
        feed_token: None,
        user_name: None,
        user_id,
    })
}
