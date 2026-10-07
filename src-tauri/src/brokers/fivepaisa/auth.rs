//! 5paisa TOTP sign-in (web `api/auth_api.py`).
//!
//! Step 1 `POST /VendorsAPI/Service1.svc/TOTPLogin`
//! `{"head":{"Key":api_key},"body":{"Email_ID","TOTP","PIN"}}` ->
//! `body.RequestToken` (refusal text in `body.Message`).
//!
//! Step 2 `POST /VendorsAPI/Service1.svc/GetAccessToken`
//! `{"head":{"Key":api_key},"body":{"RequestToken","EncryKey","UserId"}}` ->
//! `body.AccessToken`. Note the login head uses `Key`, later calls `key`.

use super::{FivepaisaBroker, Session};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// The three parts of the stored API key.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKeyParts {
    pub api_key: String,
    pub user_id: String,
    pub client_code: String,
}

impl std::fmt::Debug for ApiKeyParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyParts")
            .field("client_code", &self.client_code)
            .finish_non_exhaustive()
    }
}

/// `api_key:::user_id:::client_id` (exactly three parts, like the web's
/// tuple unpacking).
pub fn split_api_key(raw: &str) -> Result<ApiKeyParts> {
    let parts: Vec<&str> = raw.trim().split(":::").collect();
    if parts.len() != 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(AppError::Validation(
            "The 5paisa API key must be entered as api_key:::user_id:::client_id (app key, app user id and client code). Fix it on the broker settings page."
                .into(),
        ));
    }
    Ok(ApiKeyParts {
        api_key: parts[0].trim().to_string(),
        user_id: parts[1].trim().to_string(),
        client_code: parts[2].trim().to_string(),
    })
}

pub fn totp_login_body(api_key: &str, email: &str, totp: &str, pin: &str) -> Value {
    json!({
        "head": {"Key": api_key},
        "body": {"Email_ID": email, "TOTP": totp, "PIN": pin},
    })
}

pub fn access_token_body(
    api_key: &str,
    request_token: &str,
    encry_key: &str,
    user_id: &str,
) -> Value {
    json!({
        "head": {"Key": api_key},
        "body": {"RequestToken": request_token, "EncryKey": encry_key, "UserId": user_id},
    })
}

fn body_str(v: &Value, k: &str) -> Option<String> {
    v.get("body")
        .and_then(|b| b.get(k))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

async fn post(b: &FivepaisaBroker, path: &str, body: &Value) -> Result<Value> {
    let resp = b
        .http
        .post(format!("{}{}", b.base_url, path))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(
            broker = "fivepaisa",
            status = status.as_u16(),
            "5paisa sign-in step {} failed",
            path
        );
        return Err(AppError::Auth(
            "5paisa did not accept the sign-in. Check the API key settings and try again.".into(),
        ));
    }
    let (_, v): (_, Value) = http::read_json("fivepaisa", resp).await?;
    Ok(v)
}

pub async fn authenticate(b: &FivepaisaBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let keys = split_api_key(&creds.api_key)?;
    let encry_key = creds
        .api_secret
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Your 5paisa encryption key is missing. Add it as the API secret on the broker settings page."
                    .into(),
            )
        })?;
    let email = creds
        .client_id
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Validation("Enter your 5paisa login email.".into()))?;
    let pin = creds
        .password
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::Validation("Enter your 5paisa PIN.".into()))?;
    let totp = creds
        .totp
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation("Enter the TOTP from your authenticator app.".into())
        })?;

    let v = post(
        b,
        "/VendorsAPI/Service1.svc/TOTPLogin",
        &totp_login_body(&keys.api_key, &email, &totp, pin.trim()),
    )
    .await?;
    let Some(request_token) = body_str(&v, "RequestToken") else {
        let msg = body_str(&v, "Message")
            .unwrap_or_else(|| "Failed to obtain request token. Please try again.".into());
        tracing::warn!(broker = "fivepaisa", "5paisa TOTP login refused");
        return Err(AppError::Auth(format!(
            "5paisa did not accept the login: {}. Check your email, PIN and use a fresh TOTP.",
            msg
        )));
    };

    let v = post(
        b,
        "/VendorsAPI/Service1.svc/GetAccessToken",
        &access_token_body(&keys.api_key, &request_token, &encry_key, &keys.user_id),
    )
    .await?;
    let Some(access_token) = body_str(&v, "AccessToken") else {
        let msg = body_str(&v, "Message")
            .unwrap_or_else(|| "Failed to obtain access token. Please try again.".into());
        tracing::warn!(broker = "fivepaisa", "5paisa access token refused");
        return Err(AppError::Auth(format!(
            "5paisa did not issue a session: {}. Check the user id and encryption key on the broker settings page.",
            msg
        )));
    };
    let session = Session {
        api_key: keys.api_key,
        client_code: keys.client_code.clone(),
        access_token: access_token.into(),
    };
    Ok(AuthResponse {
        auth_token: session.encode(),
        feed_token: None,
        user_id: keys.client_code,
        user_name: None,
    })
}
