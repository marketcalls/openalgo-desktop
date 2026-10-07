//! AliceBlue V2 vendor sign-in (web `api/auth_api.py`).
//!
//! The callback brings `authCode` and `userId`; the catalogue passes them
//! as `userId:authCode`. `checkSum = SHA256(userId + authCode + apiSecret)`
//! is posted to `/open-api/od/v1/vendor/getUserDetails`, which answers
//! `{"stat":"Ok","userSession":<JWT>,"clientId":..}` or
//! `{"stat":"Not_ok","emsg":..}`.

use super::{text, AliceBlueBroker};
use crate::brokers::common::redact;
use crate::brokers::types::AuthToken;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256.
pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// web `checksum_input = f"{userid}{authCode}{BROKER_API_SECRET}"`.
pub fn checksum(user_id: &str, auth_code: &str, api_secret: &str) -> String {
    sha256_hex(&format!("{}{}{}", user_id, auth_code, api_secret))
}

/// Split the catalogue's `userId:authCode`.
pub fn split_code(code: &str) -> Option<(String, String)> {
    let (user, auth) = code.trim().split_once(':')?;
    let (user, auth) = (user.trim(), auth.trim());
    (!user.is_empty() && !auth.is_empty()).then(|| (user.to_string(), auth.to_string()))
}

/// The `ucc` claim of the session JWT (web order adapter fallback).
pub fn ucc_from_jwt(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let ucc = text(v.get("ucc"));
    (!ucc.is_empty()).then_some(ucc)
}

/// The client code the sockets log in with: the stored user id, else the
/// JWT's `ucc` claim (web `get_user_id` then JWT).
pub fn ucc(auth: &AuthToken) -> Option<String> {
    auth.user_id()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| ucc_from_jwt(auth.raw()))
}

pub async fn authenticate(b: &AliceBlueBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let secret = creds
        .api_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Your AliceBlue API secret is missing. Enter it in Profile, Broker Configuration."
                    .into(),
            )
        })?
        .to_string();
    let code = creds
        .auth_code
        .as_deref()
        .or(creds.request_token.as_deref())
        .unwrap_or_default();
    let (user_id, auth_code) = split_code(code).ok_or_else(|| {
        AppError::Auth("AliceBlue did not complete the sign-in. Start the login again.".into())
    })?;
    let body = json!({ "checkSum": checksum(&user_id, &auth_code, &secret) });
    let resp = b
        .http
        .post(format!(
            "{}/open-api/od/v1/vendor/getUserDetails",
            b.ep.auth
        ))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(redact::http)?;
    // Parsed here, not with the shared reader: its log of an unreadable
    // body must not see a sign-in answer.
    let bytes = resp.bytes().await.map_err(redact::http)?;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let session = text(v.get("userSession"));
    if text(v.get("stat")) == "Ok" && !session.is_empty() {
        let client_id = text(v.get("clientId"));
        return Ok(AuthResponse {
            auth_token: session,
            feed_token: None,
            user_id: if client_id.is_empty() {
                user_id
            } else {
                client_id
            },
            user_name: None,
        });
    }
    let emsg = text(v.get("emsg"));
    tracing::warn!("AliceBlue refused the sign-in");
    if emsg.is_empty() {
        return Err(AppError::Auth(
            "AliceBlue did not return a session. Check the API secret in Profile, Broker Configuration, and log in again."
                .into(),
        ));
    }
    Err(AppError::Auth(format!(
        "AliceBlue refused the sign-in: {}. Check the app code and API secret, then log in again.",
        super::mapping::describe(&emsg)
    )))
}
