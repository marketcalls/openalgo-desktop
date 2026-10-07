//! Motilal Oswal sign-in (web `api/auth_api.py`).
//!
//! 1. `POST /rest/login/v7/authdirectapi` with the common headers (no
//!    `Authorization`) and `{"userid", "password": sha256(password + apikey),
//!    "2FA": <date of birth DD/MM/YYYY>, "totp"?}` -> `AuthToken`. A login
//!    whose `isAuthTokenVerified` is explicitly negative needs the SMS/e-mail
//!    OTP path, which the web does not implement either: it is refused.
//! 2. Optional `POST /rest/login/v1/getaccesstoken` (needs the API secret)
//!    -> `accesstoken`. A failure here never fails the login.
//!
//! Form fields (web brlogin motilal branch): `userid` arrives in
//! `client_id`, `password` in `password`, the optional `totp` in `totp` and
//! the date of birth `dob` in `auth_code` (the form login's one extra
//! factor slot).

use super::{mapping::vs, MotilalBroker, MotilalSession};
use crate::brokers::common::redact;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use crate::security::Secret;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// `sha256(password + apikey)` hex (web doc 08).
pub fn password_hash(password: &str, api_key: &str) -> String {
    let mut h = Sha256::new();
    h.update(password.as_bytes());
    h.update(api_key.as_bytes());
    hex::encode(h.finalize())
}

/// Login body (web `authenticate_broker`): `totp` only when given.
pub fn login_body(userid: &str, password: &str, api_key: &str, dob: &str, totp: &str) -> Value {
    let mut body = json!({
        "userid": userid,
        "password": password_hash(password, api_key),
        "2FA": dob,
    });
    if !totp.trim().is_empty() {
        body["totp"] = Value::String(totp.trim().to_string());
    }
    body
}

/// web: only an explicitly negative `isAuthTokenVerified` blocks the login.
pub fn is_unverified(v: &Value) -> bool {
    let flag = match v.get("isAuthTokenVerified") {
        Some(Value::String(s)) => s.trim().to_ascii_uppercase(),
        Some(Value::Bool(b)) => b.to_string().to_ascii_uppercase(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    matches!(flag.as_str(), "FALSE" | "0" | "NO" | "N")
}

fn login_error(v: &Value) -> AppError {
    let code = super::error_code(v);
    let msg = vs(v, "message").unwrap_or_else(|| "Authentication failed".into());
    tracing::warn!(code = %code, "Motilal Oswal refused the login");
    let hint = match code.as_str() {
        "MO1093" => "Use a fresh code from your authenticator app.",
        "MO1007" => "Check the date of birth (DD/MM/YYYY).",
        "MO2005" => "Check the API key in Profile, Broker Configuration.",
        "MO2035" => "Register this computer's internet address as a static IP with Motilal Oswal.",
        _ => "Check your user id, password, date of birth and TOTP, then try again.",
    };
    AppError::Auth(format!(
        "Motilal Oswal did not accept the login: {}. {}",
        msg.trim_end_matches('.'),
        hint
    ))
}

fn required(v: Option<&String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Step 2: the optional access token. Never fails the login.
pub async fn access_token(
    b: &MotilalBroker,
    auth_token: &str,
    userid: &str,
    api_key: &str,
    api_secret: &str,
) -> Option<String> {
    let rb = b
        .http
        .post(format!("{}{}", b.base_url, super::paths::ACCESS_TOKEN));
    let rb = b.headers(
        rb,
        api_key,
        Some(api_secret),
        userid,
        Some((auth_token, None)),
    );
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "Motilal Oswal access-token step failed: {}",
                e.without_url()
            );
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(
            status = resp.status().as_u16(),
            "Motilal Oswal access-token step was refused; using the AuthToken alone"
        );
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    if super::is_success(&v) {
        if let Some(t) = vs(&v, "accesstoken") {
            return Some(t);
        }
    }
    tracing::warn!("Motilal Oswal returned no access token; using the AuthToken alone");
    None
}

pub async fn authenticate(b: &MotilalBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let api_key = creds.api_key.trim().to_string();
    if api_key.is_empty() {
        return Err(AppError::Validation(
            "Your Motilal Oswal API key is missing. Enter it in Profile, Broker Configuration."
                .into(),
        ));
    }
    let api_secret = required(creds.api_secret.as_ref());
    let (Some(userid), Some(password)) = (
        required(creds.client_id.as_ref()),
        creds.password.clone().filter(|p| !p.is_empty()),
    ) else {
        return Err(AppError::Validation(
            "Enter your Motilal Oswal user id and password.".into(),
        ));
    };
    let password = Secret::new(password);
    let Some(dob) = required(creds.auth_code.as_ref()) else {
        return Err(AppError::Validation(
            "Enter your date of birth as DD/MM/YYYY; Motilal Oswal uses it as the second login factor."
                .into(),
        ));
    };
    let totp = creds.totp.clone().unwrap_or_default();

    let body = login_body(&userid, password.expose(), &api_key, &dob, &totp);
    let rb = b
        .http
        .post(format!("{}{}", b.base_url, super::paths::AUTH_DIRECT));
    let rb = b.headers(rb, &api_key, api_secret.as_deref(), &userid, None);
    let resp = rb
        .body(body.to_string())
        .send()
        .await
        .map_err(redact::http)?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(status = status.as_u16(), "Motilal Oswal login failed");
        return Err(AppError::Auth(
            "Motilal Oswal could not complete the login right now. Try again in a moment.".into(),
        ));
    }
    let v: Value = resp.json().await.map_err(|_| {
        AppError::Auth(
            "Motilal Oswal sent a login answer OpenAlgo could not read. Try again.".into(),
        )
    })?;
    let token = vs(&v, "AuthToken");
    if !super::is_success(&v) || token.is_none() {
        return Err(login_error(&v));
    }
    if is_unverified(&v) {
        return Err(AppError::Auth(
            "Motilal Oswal asked for an OTP sent by SMS or e-mail, which OpenAlgo does not support yet. Enable an authenticator app with Motilal Oswal and log in with a TOTP code."
                .into(),
        ));
    }
    let auth_token = token.unwrap_or_default();
    let access = match api_secret.as_deref() {
        Some(secret) => access_token(b, &auth_token, &userid, &api_key, secret).await,
        None => {
            tracing::info!("No Motilal Oswal API secret saved; skipping the access-token step");
            None
        }
    };
    let session = MotilalSession {
        auth_token: Secret::new(auth_token),
        access_token: access.map(Secret::new),
        client_code: userid.clone(),
        api_key: Secret::new(api_key),
        api_secret: api_secret.map(Secret::new),
    };
    Ok(AuthResponse {
        auth_token: session.compose(),
        feed_token: None,
        user_id: userid,
        user_name: None,
    })
}
