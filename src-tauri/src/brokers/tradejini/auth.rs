//! Tradejini individual-app sign-in (web `api/auth_api.py`).
//!
//! `POST {base}/api-gw/oauth/individual-token-v2` with
//! `Authorization: Bearer <api_key>` and the form
//! `password=<CubePlus PIN>&twoFa=<code>&twoFaTyp=totp` ->
//! `{scope, access_token, token_type: "bearer", expires_in}`. The API secret
//! is never sent (it only belongs to the unused public-app OAuth flow).
//! A 401 is a bare "Unauthorized" for a wrong IP, key, PIN or TOTP alike,
//! so the web's ordered checklist is shown instead.

use super::TradejiniBroker;
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde_json::Value;

/// 2FA types the endpoint accepts; anything else falls back to `totp`.
pub fn twofa_type(requested: Option<&str>) -> &'static str {
    match requested.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("otp") => "otp",
        _ => "totp",
    }
}

/// web `UNAUTHORIZED_HINT`, written for the trader.
pub const UNAUTHORIZED_HINT: &str = "Tradejini did not accept the sign-in. Check, in order: the password field takes your CubePlus login PIN, not your account password; your Tradejini app only accepts requests from the static IP registered for it on the Tradejini developer portal; the API key saved in OpenAlgo must be the app's API key; and the TOTP must be current.";

/// The stored API key (web `get_api_key`: `BROKER_API_KEY`, falling back to
/// `BROKER_API_SECRET` for older setups; `YOUR_*` placeholders ignored).
pub fn api_key(creds: &BrokerCredentials) -> Option<String> {
    [Some(creds.api_key.as_str()), creds.api_secret.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|v| !v.is_empty() && !v.to_ascii_uppercase().starts_with("YOUR_"))
        .map(str::to_string)
}

pub async fn authenticate(b: &TradejiniBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let password = creds
        .password
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let twofa = creds
        .totp
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (Some(password), Some(twofa)) = (password, twofa) else {
        return Err(AppError::Validation(
            "Enter your CubePlus login PIN and the TOTP code to sign in to Tradejini.".into(),
        ));
    };
    let key = api_key(&creds).ok_or_else(|| {
        AppError::Validation(
            "Your Tradejini API key is missing. Add the app's API key from the Tradejini developer portal on the broker settings page."
                .into(),
        )
    })?;
    // Twofa type is not carried by the shared login form; the web defaults
    // to totp as well.
    let form = [
        ("password", password.to_string()),
        ("twoFa", twofa.to_string()),
        ("twoFaTyp", twofa_type(None).to_string()),
    ];
    let resp = b
        .http
        .post(format!("{}/api-gw/oauth/individual-token-v2", b.base_url))
        .header("Authorization", format!("Bearer {}", key))
        .form(&form)
        .send()
        .await?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        tracing::warn!("Tradejini login refused (401)");
        return Err(AppError::Auth(UNAUTHORIZED_HINT.into()));
    }
    let (_, v): (_, Value) = http::read_json("tradejini", resp).await?;
    if status != reqwest::StatusCode::OK {
        let msg = v
            .get("msg")
            .or_else(|| v.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Authentication failed");
        tracing::warn!(status = status.as_u16(), "Tradejini login refused");
        return Err(AppError::Auth(format!(
            "Tradejini did not accept the sign-in: {}",
            msg
        )));
    }
    let token = v
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            AppError::Auth("Tradejini did not return a session. Try signing in again.".into())
        })?;
    let token_type = v
        .get("token_type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if !token_type.is_empty() && token_type != "bearer" {
        tracing::warn!("Tradejini returned token type {}", token_type);
        return Err(AppError::Auth(
            "Tradejini returned a session OpenAlgo cannot use. Try signing in again.".into(),
        ));
    }
    Ok(AuthResponse {
        auth_token: format!("{}:{}", key, token),
        feed_token: None,
        user_id: creds.client_id.clone().unwrap_or_default(),
        user_name: None,
    })
}
