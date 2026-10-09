//! Groww sign-in (web `api/auth_api.py`, plus the TOTP and pasted-token
//! variants Groww's API offers).
//!
//! Variant order, from what the trader filled in:
//! 1. a TOTP code: `key_type: totp` with the TOTP API key;
//! 2. a pasted access token (the form's token field): validated with a
//!    funds call;
//! 3. an API secret: the web's approval checksum flow,
//!    `sha256(secret + epoch_seconds)`;
//! 4. an API key that is itself an access token (a JWT) with no secret.
//!
//! The stored session token is the raw Groww token.

use super::{error_message, in_transit, Category, GrowwCore};
use crate::brokers::types::AuthToken;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// `sha256(api_secret + timestamp)` as lowercase hex (web
/// `generate_checksum`).
pub fn checksum(api_secret: &str, timestamp: &str) -> String {
    let mut h = Sha256::new();
    h.update(api_secret.as_bytes());
    h.update(timestamp.as_bytes());
    hex::encode(h.finalize())
}

/// Whether a string has the shape of a JWT (`eyJ...` with three parts).
pub fn looks_like_jwt(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("eyJ") && s.matches('.').count() == 2
}

/// The four ways in, decided from the filled-in fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Variant {
    Totp { api_key: String, totp: String },
    PastedToken(String),
    Approval { api_key: String, api_secret: String },
}

pub fn choose_variant(creds: &BrokerCredentials) -> Result<Variant> {
    let nonempty = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let api_key = creds.api_key.trim().to_string();
    if let Some(totp) = nonempty(&creds.totp) {
        if api_key.is_empty() {
            return Err(AppError::Validation(
                "Your Groww TOTP API key is missing. Add it on the broker settings page.".into(),
            ));
        }
        return Ok(Variant::Totp { api_key, totp });
    }
    if let Some(token) = nonempty(&creds.password) {
        return Ok(Variant::PastedToken(token));
    }
    if let Some(api_secret) = nonempty(&creds.api_secret) {
        if api_key.is_empty() {
            return Err(AppError::Validation(
                "Your Groww API key is missing. Add it on the broker settings page.".into(),
            ));
        }
        return Ok(Variant::Approval {
            api_key,
            api_secret,
        });
    }
    if looks_like_jwt(&api_key) {
        return Ok(Variant::PastedToken(api_key));
    }
    Err(AppError::Validation(
        "Groww needs one of: your API key and API secret on the broker settings page, a TOTP code for a TOTP API key, or an access token pasted from Groww."
            .into(),
    ))
}

/// A login refusal in words a trader can act on, with Groww's own reason
/// (web `_login_error`): too many token requests and a failure on Groww's
/// side are not credential problems.
pub fn login_error(body: &Value, status: Option<u16>) -> AppError {
    let reason = error_message(body);
    let mut message = "Groww did not issue an access token".to_string();
    if !reason.is_empty() {
        message.push_str(": ");
        message.push_str(&reason);
    }
    match status {
        Some(429) => AppError::Broker(format!(
            "{}. Groww is limiting login requests (30 a minute, 150 a day). Wait a minute, then log in again.",
            message
        )),
        Some(s) if s >= 500 => AppError::Broker(format!(
            "{}. Groww's login service is not responding right now; the API key is not the problem. Try again in a few minutes.",
            message
        )),
        _ => AppError::Auth(format!(
            "{}. Check the API key and secret, and that the key is approved for today on Groww's API Keys page.",
            message
        )),
    }
}

/// The access token from a token response (web `_token_from_response`).
/// Groww returns `token`, `tokenRefId`, `sessionName`, `expiry` and
/// `isActive`; a token marked inactive, or already past its expiry, is
/// refused here rather than failing on the first order. A naive expiry is
/// read as IST.
pub fn token_from_response(body: &Value, now: chrono::DateTime<chrono::Utc>) -> Result<String> {
    let Some(token) = body
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        return Err(login_error(body, None));
    };
    if body.get("isActive") == Some(&Value::Bool(false)) {
        return Err(AppError::Auth(
            "Groww issued an access token that is not active. Approve the API key for today on Groww's API Keys page, then log in again."
                .into(),
        ));
    }
    if let Some(expiry) = body
        .get("expiry")
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty())
    {
        let parsed = chrono::DateTime::parse_from_rfc3339(expiry)
            .map(|d| d.with_timezone(&chrono::Utc))
            .ok()
            .or_else(|| {
                ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
                    .iter()
                    .find_map(|f| chrono::NaiveDateTime::parse_from_str(expiry, f).ok())
                    .and_then(|n| {
                        use chrono::TimeZone;
                        chrono_tz::Asia::Kolkata
                            .from_local_datetime(&n)
                            .single()
                            .map(|d| d.with_timezone(&chrono::Utc))
                    })
            });
        match parsed {
            Some(at) if at <= now => {
                return Err(AppError::Auth(format!(
                "Groww issued an access token that expired at {}. Log in again to get a new one.",
                expiry
            )))
            }
            Some(_) => tracing::info!("Groww access token valid until {}", expiry),
            None => tracing::warn!("Groww token expiry not understood: {:?}", expiry),
        }
    }
    Ok(token.to_string())
}

/// Exchange an API key for an access token (`/v1/token/api/access`, paced
/// as Authentication, 30 a minute).
async fn token_exchange(core: &GrowwCore, api_key: &str, body: Value) -> Result<String> {
    let r = core
        .request(
            Method::POST,
            "/v1/token/api/access",
            &format!("Bearer {}", api_key),
            Some(&body),
            Category::Auth,
        )
        .await
        .map_err(|e| in_transit(e, "login"))?;
    if r.status == reqwest::StatusCode::OK {
        return token_from_response(&r.body, chrono::Utc::now());
    }
    tracing::warn!(
        status = r.status.as_u16(),
        "Groww login refused: {}",
        error_message(&r.body)
    );
    Err(login_error(&r.body, Some(r.status.as_u16())))
}

/// A pasted token is checked with a funds call before it is stored.
async fn validate_token(core: &GrowwCore, token: &str) -> Result<()> {
    let auth = AuthToken::new(token);
    let r = core
        .send(
            Method::GET,
            "/v1/margins/detail/user",
            &auth,
            None,
            Category::NonTrading,
        )
        .await
        .map_err(|e| match e {
            AppError::Auth(_) => AppError::Auth(
                "Groww did not accept this access token. Generate a new one in Groww and paste it again."
                    .into(),
            ),
            other => in_transit(other, "login"),
        })?;
    if r.is_success() {
        Ok(())
    } else {
        Err(AppError::Auth(
            "Groww did not accept this access token. Generate a new one in Groww and paste it again."
                .into(),
        ))
    }
}

pub async fn authenticate(core: &GrowwCore, creds: BrokerCredentials) -> Result<AuthResponse> {
    let token = match choose_variant(&creds)? {
        Variant::Totp { api_key, totp } => {
            token_exchange(core, &api_key, json!({"key_type": "totp", "totp": totp})).await?
        }
        Variant::PastedToken(t) => {
            validate_token(core, &t).await?;
            t
        }
        Variant::Approval {
            api_key,
            api_secret,
        } => {
            let ts = chrono::Utc::now().timestamp().to_string();
            let sum = checksum(&api_secret, &ts);
            token_exchange(
                core,
                &api_key,
                json!({"key_type": "approval", "checksum": sum, "timestamp": ts}),
            )
            .await?
        }
    };
    Ok(AuthResponse {
        auth_token: token,
        feed_token: None,
        // Groww's token response carries no user id.
        user_id: creds.client_id.unwrap_or_default(),
        user_name: None,
    })
}
