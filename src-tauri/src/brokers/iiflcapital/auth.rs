//! IIFL Capital sign-in (web `api/auth_api.py`, `blueprints/brlogin.py`).
//!
//! 1. The trader is sent to
//!    `https://markets.iiflcapital.com/?v=1&appkey=<key>&redirecturl=<cb>&redirectUrl=<cb>`
//!    (both casings, callback left unescaped, exactly like the web).
//! 2. IIFL redirects back with `authCode` and `clientId` (several spellings).
//!    The desktop callback hands an adapter only one code string, so
//!    `catalog::extract_code` packs both as `<clientId>:::<authCode>`
//!    ([`CODE_SEPARATOR`]); without a `clientId` it passes the bare code and
//!    the client id falls back to the stored client id, then the stored API
//!    key (its `clientid:::appkey` prefix, or the whole key), as the web does.
//! 3. `POST {BASE}/getusersession {"checkSum": sha256(clientId + authCode +
//!    secret)}` -> `{"status":"Ok","userSession":"<jwt>"}`.

use super::IiflCapitalBroker;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Separator between the callback's client id and auth code in the code
/// string `catalog::extract_code` produces for IIFL Capital.
pub const CODE_SEPARATOR: &str = ":::";

/// `sha256(client_id + auth_code + secret)` as lowercase hex.
pub fn checksum(client_id: &str, auth_code: &str, secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(client_id.as_bytes());
    h.update(auth_code.as_bytes());
    h.update(secret.as_bytes());
    hex::encode(h.finalize())
}

/// The app key half of a stored `clientid:::appkey` API key, or the key.
pub fn app_key(api_key: &str) -> &str {
    match api_key.split_once(CODE_SEPARATOR) {
        Some((_, k)) if !k.trim().is_empty() => k.trim(),
        _ => api_key.trim(),
    }
}

/// The IIFL login URL. The callback carries `state` in its own query so the
/// desktop can verify the round trip (IIFL returns to the callback URL as
/// given); it is left unescaped like the web's `REDIRECT_URL`.
pub fn login_url(api_key: &str, redirect_url: &str, state: &str) -> String {
    let callback = format!("{}?state={}", redirect_url, urlencoding::encode(state));
    format!(
        "{}?v=1&appkey={}&redirecturl={}&redirectUrl={}",
        super::LOGIN_URL,
        urlencoding::encode(app_key(api_key)),
        callback,
        callback
    )
}

/// `(client_id, auth_code)` from the callback code and stored credentials
/// (web `brlogin.py` fallback order).
pub fn split_code(code: &str, creds: &BrokerCredentials) -> (String, String) {
    if let Some((client, auth)) = code.split_once(CODE_SEPARATOR) {
        if !client.trim().is_empty() {
            return (client.trim().to_string(), auth.trim().to_string());
        }
        return (fallback_client_id(creds), auth.trim().to_string());
    }
    (fallback_client_id(creds), code.trim().to_string())
}

fn fallback_client_id(creds: &BrokerCredentials) -> String {
    if let Some(c) = creds.client_id.as_deref().filter(|c| !c.trim().is_empty()) {
        return c.trim().to_string();
    }
    let key = creds.api_key.trim();
    match key.split_once(CODE_SEPARATOR) {
        Some((c, _)) => c.trim().to_string(),
        None => key.to_string(),
    }
}

fn message(v: &Value) -> Option<String> {
    ["message", "error"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub async fn authenticate(b: &IiflCapitalBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let secret = creds
        .api_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Add your IIFL Capital API secret in Profile, Broker Configuration, then log in again."
                    .into(),
            )
        })?
        .to_string();
    let code = creds
        .auth_code
        .as_deref()
        .or(creds.request_token.as_deref())
        .unwrap_or("")
        .to_string();
    let (client_id, auth_code) = split_code(&code, &creds);
    if auth_code.is_empty() || client_id.is_empty() {
        return Err(AppError::Auth(
            "IIFL Capital did not complete the sign-in. Check that the callback URL registered with IIFL matches OpenAlgo's exactly, then try again."
                .into(),
        ));
    }
    let body = json!({"checkSum": checksum(&client_id, &auth_code, &secret)});
    let resp = b
        .http
        .post(format!("{}/getusersession", b.base_url()))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let data: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(
                status = status.as_u16(),
                "IIFL Capital sign-in answered with a body that is not JSON"
            );
            return Err(AppError::Auth(
                "IIFL Capital could not complete the sign-in right now. Try again shortly.".into(),
            ));
        }
    };
    let ok = data
        .get("status")
        .and_then(Value::as_str)
        .map(|s| s.eq_ignore_ascii_case("ok"))
        .unwrap_or(false);
    let token = data
        .get("userSession")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (status.is_success() && ok, token) {
        (true, Some(token)) => Ok(AuthResponse {
            auth_token: token.to_string(),
            feed_token: None,
            user_id: client_id,
            user_name: None,
        }),
        _ => {
            let why = message(&data).unwrap_or_else(|| "Authentication failed".into());
            tracing::warn!(
                status = status.as_u16(),
                "IIFL Capital refused the sign-in: {}",
                why
            );
            Err(AppError::Auth(format!(
                "IIFL Capital did not accept the sign-in: {}. Check the API key and secret, then log in again.",
                why
            )))
        }
    }
}
