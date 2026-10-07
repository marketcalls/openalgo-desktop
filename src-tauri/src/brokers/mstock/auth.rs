//! mStock Type B sign-in (web `api/auth_api.py:12-138`, brlogin mstock
//! branch).
//!
//! Step 1, password: `POST {base}/connect/login` with
//! `X-Mirae-Version: 1` and `{"clientcode","password","totp","state":""}`
//! -> `data.refreshToken` (fallback `data.jwtToken`).
//!
//! Step 2, TOTP: `POST {base}/session/verifytotp` with `X-PrivateKey` and
//! `{"refreshToken","totp"}` -> `data.jwtToken` (the session) and
//! `data.feedToken`.
//!
//! Each step succeeds only when the payload `status` is `true` / `"true"`.
//! Both are public so a route can drive them as two forms; `authenticate`
//! runs both from one form, like the web's single POST.

use super::{is_success, message, read_payload, MstockBroker, MstockSession};
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// The refresh token from step 1. `Debug` is redacted.
#[derive(Clone)]
pub struct RefreshToken(pub String);

impl std::fmt::Debug for RefreshToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshToken([REDACTED])")
    }
}

/// The tokens from step 2. `Debug` is redacted.
#[derive(Clone)]
pub struct SessionTokens {
    pub jwt: String,
    pub feed_token: Option<String>,
}

impl std::fmt::Debug for SessionTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionTokens([REDACTED])")
    }
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// web accepts only `True` / `"true"` at login.
fn login_ok(v: &Value) -> bool {
    is_success(v) && v.get("data").is_some_and(|d| !d.is_null())
}

fn refused(v: &Value, step: &str) -> AppError {
    let msg = message(v);
    tracing::warn!("mStock {} refused", step);
    AppError::Auth(match step {
        "password login" => format!(
            "mStock did not accept the password or TOTP{}. Check your password and use a fresh TOTP code.",
            if msg.is_empty() { String::new() } else { format!(": {}", msg) }
        ),
        _ => format!(
            "mStock did not accept the TOTP{}. Use a fresh TOTP code and check that the API key in Profile, Broker Configuration is correct.",
            if msg.is_empty() { String::new() } else { format!(": {}", msg) }
        ),
    })
}

/// Step 1: client code + password + TOTP -> refresh token.
pub async fn password_login(
    b: &MstockBroker,
    clientcode: &str,
    password: &str,
    totp: &str,
) -> Result<RefreshToken> {
    let body = json!({
        "clientcode": clientcode,
        "password": password,
        "totp": totp.trim(),
        "state": "",
    });
    let resp = b
        .http
        .post(format!("{}/connect/login", b.base_url))
        .header("X-Mirae-Version", "1")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    let status = resp.status();
    let v = match read_payload(resp).await {
        Ok(v) => v,
        // A refused login is not an expired session.
        Err(AppError::Auth(_)) => Value::Null,
        Err(e) => return Err(e),
    };
    if !status.is_success() || !login_ok(&v) {
        return Err(refused(&v, "password login"));
    }
    let d = &v["data"];
    text(d, "refreshToken")
        .or_else(|| text(d, "jwtToken"))
        .map(RefreshToken)
        .ok_or_else(|| {
            AppError::Auth(
                "mStock accepted the password but returned no session. Try logging in again."
                    .into(),
            )
        })
}

/// Step 2: refresh token + TOTP -> session JWT and feed token.
pub async fn verify_totp(
    b: &MstockBroker,
    private_key: &str,
    refresh: &RefreshToken,
    totp: &str,
) -> Result<SessionTokens> {
    let resp = b
        .http
        .post(format!("{}/session/verifytotp", b.base_url))
        .header("X-Mirae-Version", "1")
        .header("X-PrivateKey", private_key)
        .header("Content-Type", "application/json")
        .body(json!({"refreshToken": refresh.0, "totp": totp.trim()}).to_string())
        .send()
        .await?;
    let status = resp.status();
    let v = match read_payload(resp).await {
        Ok(v) => v,
        Err(AppError::Auth(_)) => Value::Null,
        Err(e) => return Err(e),
    };
    if !status.is_success() || !login_ok(&v) {
        return Err(refused(&v, "TOTP verification"));
    }
    let d = &v["data"];
    let jwt = text(d, "jwtToken").ok_or_else(|| {
        AppError::Auth(
            "mStock accepted the TOTP but returned no session. Try logging in again.".into(),
        )
    })?;
    Ok(SessionTokens {
        jwt,
        feed_token: text(d, "feedToken"),
    })
}

pub async fn authenticate(b: &MstockBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let clientcode = creds.api_key.trim().to_string();
    if clientcode.is_empty() {
        return Err(AppError::Validation(
            "Your mStock client code is missing. Enter it as the API key in Profile, Broker Configuration."
                .into(),
        ));
    }
    let private_key = creds
        .api_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            AppError::Validation(
                "Your mStock API key is missing. Enter it as the API secret in Profile, Broker Configuration."
                    .into(),
            )
        })?;
    let password = creds.password.clone().unwrap_or_default();
    if password.is_empty() {
        return Err(AppError::Validation("Password is required.".into()));
    }
    let totp = creds.totp.clone().unwrap_or_default();
    if totp.trim().is_empty() {
        return Err(AppError::Validation("TOTP code is required.".into()));
    }
    let refresh = password_login(b, &clientcode, &password, &totp).await?;
    let tokens = verify_totp(b, &private_key, &refresh, &totp).await?;
    let session = MstockSession {
        jwt: tokens.jwt,
        private_key,
    };
    Ok(AuthResponse {
        auth_token: session.compose(),
        feed_token: tokens.feed_token,
        user_id: clientcode,
        user_name: None,
    })
}
