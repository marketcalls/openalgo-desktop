//! Upstox OAuth code exchange (web `api/auth_api.py`).
//!
//! `POST /v2/login/authorization/token`, form-encoded `code`, `client_id`,
//! `client_secret`, `redirect_uri`, `grant_type=authorization_code`. Upstox
//! refuses the exchange unless `redirect_uri` is byte-identical to the one
//! on the authorize URL; the login flow records the redirect it built the
//! login URL with and passes it back as `BrokerCredentials::redirect_uri`.

use super::{mapping, UpstoxBroker};
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde_json::Value;

/// The web convention for the shipped default port.
pub const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:5000/upstox/callback";

/// The redirect URI for the code exchange: the one the login URL was built
/// with, else the web convention.
pub fn redirect_uri(creds: &BrokerCredentials) -> String {
    creds
        .redirect_uri
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .unwrap_or(DEFAULT_REDIRECT_URI)
        .to_string()
}

/// The form `authenticate_broker` posts.
pub fn token_form(
    code: &str,
    api_key: &str,
    api_secret: &str,
    redirect: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("code", code.to_string()),
        ("client_id", api_key.to_string()),
        ("client_secret", api_secret.to_string()),
        ("redirect_uri", redirect.to_string()),
        ("grant_type", "authorization_code".to_string()),
    ]
}

pub async fn authenticate(b: &UpstoxBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let redirect = redirect_uri(&creds);
    let code = creds
        .auth_code
        .or(creds.request_token)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Upstox did not return a login code. Start the Upstox login again.".into(),
            )
        })?;
    if creds.api_key.trim().is_empty() {
        return Err(AppError::Validation(
            "Your Upstox API key is missing. Add it on the broker settings page.".into(),
        ));
    }
    let secret = creds.api_secret.filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Validation(
            "Your Upstox API secret is missing. Add it on the broker settings page.".into(),
        )
    })?;
    let form = token_form(&code, creds.api_key.trim(), &secret, &redirect);
    let resp = b
        .http
        .post(b.api("/v2/login/authorization/token"))
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        let token = body
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                AppError::Auth(
                    "Upstox accepted the login but returned no session. Log in to Upstox again."
                        .into(),
                )
            })?;
        let s = |k: &str| {
            body.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default()
        };
        return Ok(AuthResponse {
            auth_token: token.to_string(),
            feed_token: None,
            user_id: s("user_id"),
            user_name: Some(s("user_name")).filter(|n| !n.is_empty()),
        });
    }
    tracing::warn!(
        status = status.as_u16(),
        code = mapping::error_code(&body).unwrap_or_default(),
        "Upstox login refused"
    );
    let detail = mapping::error_text(&body).unwrap_or_default();
    Err(
        if detail.contains("redirect") || detail.contains("Redirect") {
            AppError::Auth(
            "Upstox refused the login because the redirect URL does not match your Upstox app. Set the app's redirect URL to the one shown in OpenAlgo, then log in again."
                .into(),
        )
        } else if detail.is_empty() {
            AppError::Auth("Upstox refused the login. Log in to Upstox again.".into())
        } else {
            AppError::Auth(format!(
                "Upstox refused the login: {}. Check your API key and secret, then log in again.",
                detail
            ))
        },
    )
}
