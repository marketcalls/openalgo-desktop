//! Request-token exchange (web `api/auth_api.py`).
//!
//! `POST /oapi/v1/access-token?api_key=<key>&request_token=<token>` with the
//! body `{"apiSecret": <secret>}` and no `Authorization` header. The answer
//! is `{"accessToken": ...}`, sometimes wrapped in `{"data": {...}}`.

use super::{client_id_from_jwt, message_of, HdfcSkyBroker, USER_AGENT};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// The access token in an exchange answer.
pub fn access_token_of(v: &Value) -> Option<String> {
    let pick = |o: &Value| {
        ["accessToken", "access_token"].iter().find_map(|k| {
            o.get(*k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    };
    pick(v).or_else(|| v.get("data").filter(|d| d.is_object()).and_then(pick))
}

pub async fn authenticate(b: &HdfcSkyBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let request_token = creds
        .request_token
        .or(creds.auth_code)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "HDFC Sky did not return a login code. Start the HDFC Sky login again.".into(),
            )
        })?;
    if creds.api_key.is_empty() {
        return Err(AppError::Validation(
            "Your HDFC Sky API key is missing. Add it on the broker settings page.".into(),
        ));
    }
    let secret = creds.api_secret.filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Validation(
            "Your HDFC Sky API secret is missing. Add it on the broker settings page.".into(),
        )
    })?;
    let resp = b
        .http
        .post(format!("{}/oapi/v1/access-token", b.urls.base))
        .query(&[
            ("api_key", creds.api_key.as_str()),
            ("request_token", request_token.as_str()),
        ])
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json")
        .json(&json!({ "apiSecret": secret }))
        .send()
        .await
        .map_err(|e| AppError::from(e.without_url()))?;
    let (status, v): (_, Value) = http::read_json("hdfcsky", resp)
        .await
        .map_err(super::redact)?;
    let Some(token) = access_token_of(&v) else {
        let msg = message_of(&v);
        tracing::warn!(status = status.as_u16(), "HDFC Sky login refused: {}", msg);
        return Err(AppError::Auth(if msg.is_empty() {
            "HDFC Sky did not accept the login. Check your API key and secret, then log in again."
                .into()
        } else {
            format!(
                "HDFC Sky did not accept the login: {}. Log in to HDFC Sky again.",
                msg
            )
        }));
    };
    let user_id = client_id_from_jwt(&token)
        .or(creds.client_id)
        .unwrap_or_default();
    Ok(AuthResponse {
        // The api_key is needed as a query parameter on every later call.
        auth_token: format!("{}:{}", creds.api_key, token),
        feed_token: None,
        user_id,
        user_name: None,
    })
}
