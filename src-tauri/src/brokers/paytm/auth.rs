//! Paytm Money token exchange (web `api/auth_api.py`).
//!
//! `POST /accounts/v2/gettoken` with `{api_key, api_secret_key,
//! request_token}`. The answer carries `access_token` (REST),
//! `public_access_token` (market-data socket, stored as the feed token) and
//! `read_access_token` (unused). Without a public token the access token
//! serves both, as on the web.

use super::PaytmBroker;
use crate::brokers::common::de::string_lenient;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TokenResponse {
    #[serde(deserialize_with = "string_lenient")]
    pub access_token: String,
    #[serde(deserialize_with = "string_lenient")]
    pub public_access_token: String,
    #[serde(deserialize_with = "string_lenient")]
    pub message: String,
    pub errors: Value,
}

impl TokenResponse {
    fn errors_text(&self) -> String {
        let list: Vec<&str> = self
            .errors
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.get("message").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        if list.is_empty() {
            self.message.clone()
        } else {
            list.join("; ")
        }
    }
}

/// The session the web stores: (access token, feed token).
pub fn session_from(resp: &TokenResponse) -> Option<(String, String)> {
    let access = resp.access_token.trim();
    if access.is_empty() {
        return None;
    }
    let feed = resp.public_access_token.trim();
    let feed = if feed.is_empty() { access } else { feed };
    Some((access.to_string(), feed.to_string()))
}

pub async fn authenticate(b: &PaytmBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let request_token = creds
        .request_token
        .or(creds.auth_code)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Paytm Money did not return a login code. Start the Paytm Money login again."
                    .into(),
            )
        })?;
    if creds.api_key.is_empty() {
        return Err(AppError::Validation(
            "Your Paytm Money API key is missing. Add it on the broker settings page.".into(),
        ));
    }
    let api_secret = creds.api_secret.filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Validation(
            "Your Paytm Money API secret is missing. Add it on the broker settings page.".into(),
        )
    })?;
    let body = json!({
        "api_key": creds.api_key,
        "api_secret_key": api_secret,
        "request_token": request_token,
    });
    let resp = b
        .http
        .post(format!("{}/accounts/v2/gettoken", b.urls.api))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| super::redact(e.into()))?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| super::redact(e.into()))?;
    let parsed: TokenResponse = serde_json::from_slice(&bytes).unwrap_or_default();
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            "Paytm Money login refused: {}",
            parsed.errors_text()
        );
        return Err(if status.is_server_error() {
            AppError::Auth(
                "Paytm Money's login service is not responding normally. Try again shortly.".into(),
            )
        } else {
            AppError::Auth(
                "Paytm Money did not accept the login. The login code may have expired, or the API key or secret is wrong. Check them on the broker settings page and log in again."
                    .into(),
            )
        });
    }
    let (auth_token, feed) = session_from(&parsed).ok_or_else(|| {
        tracing::warn!("Paytm Money login answered without an access token");
        AppError::Auth(
            "Paytm Money accepted the login but returned no session. Log in to Paytm Money again."
                .into(),
        )
    })?;
    Ok(AuthResponse {
        auth_token,
        feed_token: Some(feed),
        // The token answer names no account; the web stores none either.
        user_id: String::new(),
        user_name: None,
    })
}
