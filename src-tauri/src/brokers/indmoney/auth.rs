//! INDstocks sign-in (web `api/auth_api.py`, `blueprints/brlogin.py`).
//!
//! * Stored secret set: a pasted 24-hour access token. It is checked with
//!   `GET /user/profile`; 200/201 accepts it, 401/403 rejects it, anything
//!   else (network, 429, 5xx) cannot decide and the token is used anyway.
//!   A rejected token falls through to the TOTP flow when the form carries
//!   an MPIN and a TOTP.
//! * TOTP: `POST /generate/token` with `x-api-key: <client id>` and
//!   `{"mpin", "totp"}` (TOTP kept as a string so leading zeros survive) ->
//!   `data.token` (also `data.access_token`, top-level `token` /
//!   `access_token`). Throttled to 1 per 60 s with lockouts after wrong
//!   codes, so it is never retried and never paced through the shared clock.

use super::{IndmoneyBroker, Reply};
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

/// Outcome of checking a pasted token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenCheck {
    Valid,
    Rejected(String),
    /// The broker could not be asked; the token is not proven bad.
    Unknown,
}

pub fn classify_profile(status: u16, body: &Value) -> TokenCheck {
    match status {
        200 | 201 => TokenCheck::Valid,
        401 | 403 => TokenCheck::Rejected(
            body.get("message")
                .or_else(|| body.get("error_type"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("HTTP {}", status)),
        ),
        _ => TokenCheck::Unknown,
    }
}

/// The token in a `/generate/token` answer.
pub fn token_from(body: &Value) -> Option<String> {
    let data = body.get("data");
    [
        data.and_then(|d| d.get("token")),
        data.and_then(|d| d.get("access_token")),
        body.get("token"),
        body.get("access_token"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::trim)
    .find(|s| !s.is_empty())
    .map(str::to_string)
}

/// Trader-facing message for a refused `/generate/token` (web
/// `_http_error_message` + `_decorate_error`).
pub fn totp_error(status: u16, body: &Value) -> String {
    let base = body
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| body.get("error").and_then(Value::as_str))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| match status {
            401 | 403 => "Invalid Client ID, MPIN, or TOTP code.".to_string(),
            429 => "Token generation is throttled.".to_string(),
            s if s >= 500 => "INDstocks is temporarily unavailable. Try again shortly.".to_string(),
            _ => "INDmoney did not issue a session.".to_string(),
        });
    match status {
        429 => format!(
            "{} Token generation is limited to 1 request per 60 seconds. Wait a minute before trying again.",
            base
        ),
        401 | 403 => format!(
            "{} Check the Client ID saved as your API key and your MPIN, and wait for a fresh TOTP code. Never resubmit a code you already used: 5 wrong codes in 15 minutes locks token generation for 15 minutes. If codes keep failing, sync your computer clock.",
            base
        ),
        _ => base,
    }
}

async fn check_pasted(b: &IndmoneyBroker, token: &str) -> TokenCheck {
    match b.send(Method::GET, "/user/profile", &[], None, token).await {
        Ok(Reply { status, json, .. }) => classify_profile(status, &json),
        Err(e) => {
            tracing::warn!("Could not validate the INDmoney access token: {}", e.code());
            TokenCheck::Unknown
        }
    }
}

async fn generate_token(
    b: &IndmoneyBroker,
    client_id: &str,
    mpin: &str,
    totp: &str,
) -> Result<String> {
    let resp = b
        .http
        .post(b.url("/generate/token"))
        .header("x-api-key", client_id)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(json!({"mpin": mpin, "totp": totp}).to_string())
        .send()
        .await?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await?;
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            tracing::error!(status, "INDmoney /generate/token answered without JSON");
            return Err(AppError::Auth(totp_error(status, &Value::Null)));
        }
    };
    if status == 200 || status == 201 {
        return token_from(&body).ok_or_else(|| {
            AppError::Auth("INDmoney accepted the login but returned no session. Try again.".into())
        });
    }
    tracing::warn!(status, "INDmoney TOTP sign-in refused");
    Err(AppError::Auth(totp_error(status, &body)))
}

pub async fn authenticate(b: &IndmoneyBroker, c: BrokerCredentials) -> Result<AuthResponse> {
    let client_id = c.api_key.trim().to_string();
    let pasted = c
        .api_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with("YOUR_"))
        .map(str::to_string);
    let mpin = c
        .password
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let totp = c.totp.as_deref().map(str::trim).unwrap_or("").to_string();
    let user_id = if client_id.is_empty() {
        "indmoney".to_string()
    } else {
        client_id.clone()
    };
    let done = |token: String| AuthResponse {
        auth_token: token,
        feed_token: None,
        user_id: user_id.clone(),
        user_name: None,
    };

    let mut rejected: Option<String> = None;
    if let Some(tok) = pasted {
        match check_pasted(b, &tok).await {
            TokenCheck::Valid => return Ok(done(tok)),
            TokenCheck::Unknown => {
                tracing::warn!("INDmoney access token could not be verified; using it");
                return Ok(done(tok));
            }
            TokenCheck::Rejected(reason) => {
                tracing::warn!("Stored INDmoney access token was rejected: {}", reason);
                rejected = Some(reason);
            }
        }
    }

    if client_id.is_empty() {
        return Err(AppError::Validation(
            "Save your INDstocks Client ID as the API key (shown on the INDstocks access-tokens page after TOTP setup), then log in again."
                .into(),
        ));
    }
    if mpin.is_empty() || totp.is_empty() {
        return Err(AppError::Validation(match rejected {
            Some(_) => "The access token saved for INDmoney has expired (tokens last 24 hours). Enter your MPIN and a fresh TOTP code, or paste a new token from INDstocks.".into(),
            None => "Enter your INDmoney MPIN and the 6-digit TOTP code.".into(),
        }));
    }
    let token = generate_token(b, &client_id, &mpin, &totp).await?;
    Ok(done(token))
}
