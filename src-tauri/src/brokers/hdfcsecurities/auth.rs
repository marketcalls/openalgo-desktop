//! Request-token exchange (web `api/auth_api.py`).
//!
//! `POST /oapi/v1/access-token?api_key=<key>&request_token=<token>` with the
//! JSON body `{"apiSecret": <secret>}` and no `Authorization` header. The
//! answer is `{"accessToken": ..}`, or the same inside `data`.

use super::{error_message, HdfcSecuritiesBroker, USER_AGENT};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use base64::Engine;
use serde_json::{json, Value};

/// `accessToken` / `access_token`, top level or under `data`.
pub fn access_token(v: &Value) -> Option<String> {
    let pick = |o: &Value| {
        ["accessToken", "access_token"]
            .iter()
            .find_map(|k| o.get(*k).and_then(Value::as_str))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    pick(v).or_else(|| v.get("data").filter(|d| d.is_object()).and_then(pick))
}

/// The `sub` claim of a JWT (the InvestRight client id), if readable.
pub fn jwt_subject(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    match v.get("sub")? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub async fn authenticate(
    b: &HdfcSecuritiesBroker,
    creds: BrokerCredentials,
) -> Result<AuthResponse> {
    let request_token = creds
        .request_token
        .or(creds.auth_code)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "HDFC Securities did not return a login code. Start the HDFC Securities login again."
                    .into(),
            )
        })?;
    let secret = creds.api_secret.filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Validation(
            "Your HDFC Securities API secret is missing. Add it on the broker settings page."
                .into(),
        )
    })?;
    if creds.api_key.is_empty() {
        return Err(AppError::Validation(
            "Your HDFC Securities API key is missing. Add it on the broker settings page.".into(),
        ));
    }
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
        .map_err(|e| {
            tracing::warn!("HDFC Securities login request failed: {}", e.without_url());
            AppError::Auth(
                "Could not reach HDFC Securities to finish the login. Check your internet connection and try again."
                    .into(),
            )
        })?;
    let (status, v): (_, Value) = http::read_json("hdfcsecurities", resp)
        .await
        .map_err(super::redact)?;
    let Some(token) = access_token(&v) else {
        tracing::warn!(
            status = status.as_u16(),
            "HDFC Securities login refused: {}",
            error_message(&v)
        );
        let m = error_message(&v);
        return Err(AppError::Auth(if m.is_empty() {
            "HDFC Securities did not accept the login. Check your API key and secret on the broker settings page, then log in again."
                .into()
        } else {
            format!("HDFC Securities did not accept the login: {}", m)
        }));
    };
    Ok(AuthResponse {
        user_id: jwt_subject(&token).unwrap_or_default(),
        // Stored as `api_key:access_token`: every call needs both.
        auth_token: format!("{}:{}", creds.api_key, token),
        feed_token: None,
        user_name: None,
    })
}
