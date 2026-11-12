//! Sahi sign-in (web `broker/sahi/api/auth_api.py`).
//!
//! `POST {auth host}/api/v1/apikeys/token` with `{key_id, secret}` returns
//! the day's Bearer token, valid until the next 06:00 IST. The account is
//! then read from `GET /v1/profile` (`user_id`, `name`); a key without the
//! profile scope still signs in, and the session account falls back to the
//! saved keys' identity.
//!
//! A key is Regular (approved in Sahi's API Dashboard each trading day; the
//! mint sends no `totp`, which Sahi requires) or TOTP (the first correct
//! authenticator code of the day approves it; every mint carries the current
//! 6-digit code). The login card asks for the code, optionally, as for
//! Groww: blank for a Regular key, typed for a TOTP key. Nothing about the
//! TOTP is stored.

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
/// auth host answers `{code, message}` with a PascalCase code: InvalidRequest
/// (400), Unauthorized, InvalidTotp or Locked (401), Forbidden (403: a
/// Regular key not approved today, or API keys not enabled for the account
/// yet) and TooManyRequests (429, also after too many wrong codes).
pub fn login_error(status: StatusCode, body: &Value) -> AppError {
    let code = text(body, "code").unwrap_or_default();
    tracing::warn!(
        broker = "sahi",
        status = status.as_u16(),
        code = code.as_str(),
        "Sahi did not issue an access token"
    );
    let message = if code == "InvalidTotp" {
        "Sahi did not accept the TOTP code. Enter the code your authenticator app shows for this API key now (it changes every 30 seconds), and check that your phone's clock is set automatically.".to_string()
    } else if status == StatusCode::FORBIDDEN || code == "Forbidden" {
        "Sahi did not issue an access token. Either the API key is not approved for today (open Sahi web, go to Profile, API Dashboard, and click Approve on the key), or API keys are not enabled for your Sahi account yet (contact Sahi). Then log in again. A TOTP key needs no Approve: enter its code when you log in.".to_string()
    } else if status == StatusCode::BAD_REQUEST && code == "InvalidRequest" {
        format!(
            "Sahi refused the login request{}. A TOTP key needs its 6-digit code when you log in; for a Regular key leave the code blank.",
            text(body, "message").map(|m| format!(": {}", m)).unwrap_or_default()
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

/// The code typed on the login card: `None` when blank (a Regular key),
/// else six digits, sent as a string so a leading zero stays. Anything else
/// is refused before the mint is sent.
pub fn totp_code(typed: Option<&str>) -> Result<Option<String>> {
    let code: String = typed
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if code.is_empty() {
        return Ok(None);
    }
    if code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Some(code));
    }
    Err(AppError::Validation(
        "Enter the 6-digit code from your authenticator app, or leave the code blank for a Regular key.".into(),
    ))
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
    let mut body = json!({ "key_id": key_id, "secret": secret });
    if let Some(code) = totp_code(creds.totp.as_deref())? {
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
