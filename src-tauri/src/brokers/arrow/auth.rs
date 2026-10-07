//! Arrow request-token exchange (web `api/auth_api.py`).

use super::{message_of, ArrowBroker};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// `SHA256("appID:appSecret:request-token")` as hex: colon separated, in
/// this order (`auth_api.py:41-43`; Kite concatenates without separators).
pub fn checksum(app_id: &str, app_secret: &str, request_token: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("{}:{}:{}", app_id, app_secret, request_token).as_bytes());
    hex::encode(h.finalize())
}

pub async fn authenticate(b: &ArrowBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let request_token = creds
        .request_token
        .or(creds.auth_code)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Arrow did not return a login code. Start the Arrow login again.".into(),
            )
        })?;
    if creds.api_key.is_empty() {
        return Err(AppError::Validation(
            "Your Arrow app ID is missing. Add it as the API key on the broker settings page."
                .into(),
        ));
    }
    let secret = creds.api_secret.filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Validation(
            "Your Arrow app secret is missing. Add it as the API secret on the broker settings page."
                .into(),
        )
    })?;
    let body = json!({
        "appID": creds.api_key,
        "token": request_token,
        "checkSum": checksum(&creds.api_key, &secret, &request_token),
    });
    let resp = b
        .http
        .post(format!("{}/auth/app/authenticate-token", b.urls.rest))
        .json(&body)
        .send()
        .await
        .map_err(|e| super::redact(e.into()))?;
    let (status, v): (_, Value) = http::read_json("arrow", resp)
        .await
        .map_err(super::redact)?;
    if v.get("status").and_then(Value::as_str) != Some("success") || !status.is_success() {
        let msg = message_of(&v);
        tracing::warn!(status = status.as_u16(), "Arrow login refused: {}", msg);
        return Err(AppError::Auth(if msg.is_empty() {
            "Arrow did not accept the login. Check your app ID and secret, then log in again."
                .into()
        } else {
            format!("Arrow did not accept the login: {}", msg)
        }));
    }
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let jwt = data
        .get("token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Auth(
                "Arrow accepted the login but returned no session. Log in to Arrow again.".into(),
            )
        })?;
    let s = |k: &str| {
        data.get(k)
            .map(|x| match x {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                o => o.to_string(),
            })
            .unwrap_or_default()
    };
    let name = s("name");
    Ok(AuthResponse {
        auth_token: format!("{}:{}", creds.api_key, jwt),
        // The feeds authenticate with the JWT half of `auth_token`.
        feed_token: None,
        user_id: s("userID"),
        user_name: (!name.is_empty()).then_some(name),
    })
}
