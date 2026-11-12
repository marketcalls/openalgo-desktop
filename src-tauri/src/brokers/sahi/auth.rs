//! Sahi sign-in (web `broker/sahi/api/auth_api.py`).
//!
//! `POST {auth host}/api/v1/apikeys/token` with `{key_id, secret}` returns
//! the day's Bearer token, valid until the next 06:00 IST. The account is
//! then read from `GET /v1/profile` (`user_id`, `name`); a key without the
//! profile scope still signs in, and the session account falls back to the
//! saved keys' identity.
//!
//! A key is one of two types, fixed when it is created (web #2441):
//! a Regular key is approved in Sahi's API Dashboard each trading day and
//! its mint sends no `totp` (Sahi refuses a Regular key with one); a TOTP
//! key approves itself, and every mint carries the current 6-digit
//! authenticator code in `totp`. The login card asks for that code, blank
//! for a Regular key (`catalog::login_fields`); nothing about it is stored.

use super::ratelimit::{Category, Op};
use super::{Call, SahiBroker};
use crate::brokers::types::AuthToken;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

pub const TOKEN_PATH: &str = "/api/v1/apikeys/token";

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A login refusal in words a trader can act on (web `_login_error`). The
/// auth host answers `{code, message}` with a PascalCase code:
/// InvalidRequest (400), Unauthorized, InvalidTotp or Locked (401),
/// Forbidden (403: a Regular key not approved today, or API keys not
/// enabled for the account yet) and TooManyRequests (429, also after too
/// many wrong codes). `server_time` is logged: a wrong code is often a
/// phone clock that drifted.
pub fn login_error(status: StatusCode, body: &Value) -> AppError {
    let code = text(body, "code").unwrap_or_default();
    tracing::warn!(
        broker = "sahi",
        status = status.as_u16(),
        code = code.as_str(),
        server_time = text(body, "server_time").as_deref().unwrap_or(""),
        "Sahi did not issue an access token"
    );
    let message = if code == "InvalidTotp" {
        "Sahi did not accept the TOTP code. Enter the code your authenticator app shows for this API key now (it changes every 30 seconds), and check that your phone's clock is set automatically.".to_string()
    } else if status == StatusCode::FORBIDDEN || code == "Forbidden" {
        "Sahi did not issue an access token. Either the API key is not approved for today (open Sahi web, go to Profile, API Dashboard, and click Approve on the key), or API keys are not enabled for your Sahi account yet (contact Sahi). Then log in again. A TOTP key needs no Approve: enter its code when you log in.".to_string()
    } else if status == StatusCode::BAD_REQUEST && code == "InvalidRequest" {
        format!(
            "Sahi refused the login request{}. A TOTP key needs its 6-digit code when you log in; for a Regular key leave the code blank.",
            text(body, "message")
                .map(|reason| format!(": {}", reason))
                .unwrap_or_default()
        )
    } else if code == "Locked" {
        "Sahi reports that your account is frozen or locked. Contact Sahi support to unlock it."
            .to_string()
    } else if status == StatusCode::UNAUTHORIZED {
        "Sahi did not accept the API key and secret. Check the API key (the key id starting with pk_) and the API secret (starting with sahi_sk_) on the Broker Configuration page, and that the key has not been deleted in the Sahi API Dashboard.".to_string()
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        "Sahi is limiting login attempts, which also happens after several wrong TOTP codes. Wait a minute, then log in again.".to_string()
    } else if status.is_server_error() {
        "Sahi's login service is not responding right now. Try again in a few minutes.".to_string()
    } else {
        match text(body, "message") {
            Some(reason) => format!(
                "Sahi did not issue an access token: {}. Check the API key and secret on the Broker Configuration page.",
                reason
            ),
            None => "Sahi did not issue an access token. Check the API key and secret on the Broker Configuration page.".to_string(),
        }
    };
    AppError::Auth(message)
}

/// The code from the login card (web `authenticate_broker`'s `totp`):
/// `None` for a Regular key (blank), else exactly six digits, spaces
/// removed. Sent as a string so a leading zero stays; anything else is
/// refused before a request is sent.
fn totp_code(typed: Option<&str>) -> Result<Option<String>> {
    let code: String = typed
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if code.is_empty() {
        return Ok(None);
    }
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AppError::Validation(
            "Enter the 6-digit code from your authenticator app, or leave the code blank for a Regular key."
                .into(),
        ));
    }
    Ok(Some(code))
}

fn required(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub async fn authenticate(b: &SahiBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let (Some(key_id), Some(secret)) = (
        required(Some(&creds.api_key)),
        required(creds.api_secret.as_deref()),
    ) else {
        return Err(AppError::Validation(
            "Sahi needs the API key (the key id, pk_...) and the API secret (sahi_sk_...). Add both on the Broker Configuration page.".into(),
        ));
    };
    let totp = totp_code(creds.totp.as_deref())?;
    let mut body = json!({ "key_id": key_id, "secret": secret });
    if let Some(code) = &totp {
        body["totp"] = json!(code);
    }
    let call = Call {
        category: Category::Auth,
        url: format!("{}{}", b.auth_url, TOKEN_PATH),
        bearer: None,
        method: Method::POST,
        query: &[],
        body: Some(&body),
    };
    let reply = b.send_to(&call, &Op::single()).await?;
    let token = match text(&reply.body, "access_token") {
        Some(t) if reply.status == StatusCode::OK => t,
        _ => return Err(login_error(reply.status, &reply.body)),
    };
    tracing::info!(
        key = if totp.is_some() { "TOTP" } else { "Regular" },
        expires_at = text(&reply.body, "expires_at").as_deref().unwrap_or(""),
        "Sahi access token issued"
    );
    let (user_id, user_name) = profile(b, &token).await;
    Ok(AuthResponse {
        auth_token: token,
        feed_token: None,
        user_id,
        user_name,
    })
}

/// `(user_id, name)` from the profile; the broker id when it cannot be read
/// (a key without `profile.read`), which the session account treats as no
/// account named.
async fn profile(b: &SahiBroker, token: &str) -> (String, Option<String>) {
    let auth = AuthToken::new(token);
    match b.get(&auth, "/v1/profile", &[], &Op::single()).await {
        Ok(v) => (
            text(&v, "user_id").unwrap_or_else(|| "sahi".to_string()),
            text(&v, "name"),
        ),
        Err(e) => {
            tracing::warn!("Sahi profile could not be read: {}", e.code());
            ("sahi".to_string(), None)
        }
    }
}
