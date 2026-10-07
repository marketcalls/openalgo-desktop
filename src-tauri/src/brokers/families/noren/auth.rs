//! Noren sign-in (web `api/auth_api.py` of each member).
//!
//! * GenAcsTok (shoonya, zebu, tradesmart): `POST {rest}/GenAcsTok`,
//!   `text/plain`, `jData={"code":..,"checksum":sha256(client_id+secret+code)}`.
//! * ApiToken (flattrade): `POST https://authapi.flattrade.in/trade/apitoken`,
//!   JSON `{"api_key","request_code","api_secret":sha256(api_key+code+secret)}`.
//!
//! The stored token is `uid:::access_token`.

use super::transport::encode_jdata;
use super::{Login, NorenBroker};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn sha256_hex(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
    }
    hex::encode(h.finalize())
}

/// `userid:::key` -> (userid, key). A key without `:::` is the app key and
/// the user id comes from the separate client-id field.
pub fn split_api_key(api_key: &str, client_id: Option<&str>) -> (String, String) {
    match api_key.split_once(":::") {
        Some((uid, key)) => (uid.trim().to_string(), key.trim().to_string()),
        None => (
            client_id.unwrap_or_default().trim().to_string(),
            api_key.trim().to_string(),
        ),
    }
}

/// The token-exchange request: (body, content type).
pub fn exchange_request(
    login: Login,
    app_key: &str,
    secret: &str,
    code: &str,
) -> (String, &'static str) {
    match login {
        Login::GenAcsTok { .. } => {
            let checksum = sha256_hex(&[app_key, secret, code]);
            (
                format!(
                    "jData={}",
                    encode_jdata(&json!({"code": code, "checksum": checksum}))
                ),
                "text/plain",
            )
        }
        Login::ApiToken { .. } => (
            json!({
                "api_key": app_key,
                "request_code": code,
                "api_secret": sha256_hex(&[app_key, code, secret]),
            })
            .to_string(),
            "application/json",
        ),
    }
}

pub async fn authenticate(b: &NorenBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let name = b.cfg.name;
    let has_code = creds
        .request_token
        .as_deref()
        .or(creds.auth_code.as_deref())
        .is_some_and(|c| !c.is_empty());
    if let (false, Some(token)) = (
        has_code,
        creds.password.as_deref().filter(|t| !t.is_empty()),
    ) {
        // A ready access token (web tradesmart manual fallback): used as
        // is, with the user id from the callback or the stored key.
        let (uid, _) = split_api_key(&creds.api_key, creds.client_id.as_deref());
        let uid = creds
            .client_id
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or(uid);
        if uid.is_empty() {
            return Err(AppError::Validation(format!(
                "Add your {} trading user id: enter the API key as userid:::key, or fill in the client id field.",
                name
            )));
        }
        return Ok(AuthResponse {
            auth_token: format!("{}:::{}", uid, token.trim()),
            feed_token: None,
            user_id: uid,
            user_name: None,
        });
    }
    let code = creds
        .request_token
        .clone()
        .or(creds.auth_code.clone())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(format!(
                "{} did not return a login code. Start the {} login again.",
                name, name
            ))
        })?;
    let secret = creds
        .api_secret
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Your {} API secret is missing. Add it on the broker settings page.",
                name
            ))
        })?;
    let (uid, app_key) = split_api_key(&creds.api_key, creds.client_id.as_deref());
    if app_key.is_empty() {
        return Err(AppError::Validation(format!(
            "Your {} API key is missing. Add it on the broker settings page.",
            name
        )));
    }
    let (body, content_type) = exchange_request(b.cfg.login, &app_key, &secret, &code);
    let resp = b
        .http
        .post(&b.endpoints.token)
        .header("Content-Type", content_type)
        .body(body)
        .send()
        .await?;
    let (_, v): (_, Value) = http::read_json(b.cfg.id, resp).await?;
    if v.get("stat").and_then(Value::as_str) != Some("Ok") {
        let emsg = super::transport::emsg(&v);
        tracing::warn!(broker = b.cfg.id, "{} login refused: {}", name, emsg);
        return Err(AppError::Auth(if emsg.trim().is_empty() {
            format!(
                "{} did not accept the login. Check your API key and secret, then log in again.",
                name
            )
        } else {
            format!("{} did not accept the login: {}", name, emsg.trim())
        }));
    }
    let (token, returned_uid) = (b.cfg.hooks.parse_login)(&v).ok_or_else(|| {
        AppError::Auth(format!(
            "{} accepted the login but returned no session. Log in to {} again.",
            name, name
        ))
    })?;
    let uid = returned_uid.filter(|u| !u.is_empty()).unwrap_or(uid);
    if uid.is_empty() {
        return Err(AppError::Validation(format!(
            "Add your {} trading user id: enter the API key as userid:::key, or fill in the client id field.",
            name
        )));
    }
    Ok(AuthResponse {
        auth_token: format!("{}:::{}", uid, token),
        feed_token: None,
        user_id: uid,
        user_name: None,
    })
}
