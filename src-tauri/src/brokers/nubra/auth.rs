//! Nubra sign-in (web `api/auth_api.py`).
//!
//! TOTP path (what `authenticate` runs, web `authenticate_broker_totp`):
//! 1. `POST /totp/login` with `Content-Type` and `x-device-id`, body
//!    `{"phone", "totp", "otp": ""}` -> `auth_token`. `totp` goes as a JSON
//!    integer (as documented); a code with a leading zero is retried as the
//!    zero-padded string within the same 30 s window.
//! 2. `POST /verifypin` with `Authorization: Bearer <auth_token>` and
//!    `x-device-id`, body `{"pin": "<mpin>"}` -> `session_token`.
//!
//! Phone-OTP path (web default, two forms): `send_phone_otp` (once, and a
//! second time with `skip_totp: true` when `next == VERIFY_TOTP`),
//! `verify_phone_otp`, then `verify_pin`. Its first half needs a route that
//! asks Nubra to send the SMS before the form is shown; until that route
//! exists these are exposed for it.

use super::{error_text, NubraBroker, DEVICE_ID};
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// Response of `/sendphoneotp`. `Debug` is redacted.
#[derive(Clone)]
pub struct OtpChallenge {
    pub temp_token: String,
    pub next: String,
}

impl std::fmt::Debug for OtpChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtpChallenge")
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

/// web `_normalize_totp`: 1-6 digits, zero-padded to 6.
pub fn normalize_totp(code: &str) -> Option<String> {
    let s = code.trim();
    if s.is_empty() || s.len() > 6 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{:0>6}", s))
}

/// The `totp` values to try, in order (web `_totp_login`): the integer,
/// then the zero-padded string when the integer drops a leading zero.
pub fn totp_candidates(totp: &str) -> Vec<Value> {
    let n: u64 = totp.parse().unwrap_or(0);
    let mut v = vec![json!(n)];
    if totp != n.to_string() {
        v.push(json!(totp));
    }
    v
}

async fn post(
    b: &NubraBroker,
    path: &str,
    headers: &[(&str, String)],
    body: &Value,
) -> Result<(u16, Value)> {
    let mut req = b
        .http
        .post(b.url(path))
        .header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = req.body(body.to_string()).send().await?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await?;
    Ok((
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    ))
}

fn token(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Step 1 of the TOTP path: `auth_token`.
pub async fn totp_login(b: &NubraBroker, phone: &str, totp: &str) -> Result<String> {
    let mut last = Value::Null;
    for candidate in totp_candidates(totp) {
        let (_, v) = post(
            b,
            "/totp/login",
            &[("x-device-id", DEVICE_ID.to_string())],
            &json!({"phone": phone, "totp": candidate, "otp": ""}),
        )
        .await?;
        if let Some(t) = token(&v, "auth_token") {
            return Ok(t);
        }
        last = v;
    }
    let why = error_text(&last).unwrap_or_else(|| "TOTP login failed".into());
    tracing::warn!("Nubra TOTP login refused");
    Err(AppError::Auth(format!(
        "Nubra did not accept the TOTP: {}. Check that TOTP is enabled on your Nubra account and use a fresh code.",
        why
    )))
}

/// Final step of both paths: exchange the login `auth_token` and the MPIN
/// for the session token. No `x-temp-token` here.
pub async fn verify_pin(b: &NubraBroker, auth_token: &str, mpin: &str) -> Result<String> {
    let (status, v) = post(
        b,
        "/verifypin",
        &[
            ("Authorization", format!("Bearer {}", auth_token)),
            ("Accept", "application/json".to_string()),
            ("x-device-id", DEVICE_ID.to_string()),
        ],
        &json!({"pin": mpin}),
    )
    .await?;
    if status != 200 {
        let why = error_text(&v).unwrap_or_else(|| "PIN verification failed".into());
        tracing::warn!(status, "Nubra MPIN verification refused");
        return Err(AppError::Auth(format!(
            "Nubra did not accept the MPIN: {}. Check the MPIN saved as the API secret.",
            why
        )));
    }
    token(&v, "session_token").ok_or_else(|| {
        AppError::Auth("Nubra did not return a session after the MPIN check. Try again.".into())
    })
}

/// `POST /sendphoneotp` (steps 1 and 2 of the phone path). Sent without a
/// device id, as the official SDK does.
pub async fn send_phone_otp(
    b: &NubraBroker,
    phone: &str,
    temp_token: Option<&str>,
    skip_totp: bool,
) -> Result<OtpChallenge> {
    let mut headers = Vec::new();
    if let Some(t) = temp_token {
        headers.push(("x-temp-token", t.to_string()));
    }
    let (_, v) = post(
        b,
        "/sendphoneotp",
        &headers,
        &json!({"phone": phone, "flow": "", "skip_totp": skip_totp}),
    )
    .await?;
    match token(&v, "temp_token") {
        Some(t) => Ok(OtpChallenge {
            temp_token: t,
            next: v
                .get("next")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_uppercase(),
        }),
        None => Err(AppError::Auth(format!(
            "Nubra could not send the login OTP: {}.",
            error_text(&v).unwrap_or_else(|| "request refused".into())
        ))),
    }
}

/// web `request_login_otp`: start the phone login and return the token the
/// OTP is redeemed with.
pub async fn request_login_otp(b: &NubraBroker, phone: &str) -> Result<OtpChallenge> {
    let first = send_phone_otp(b, phone, None, false).await?;
    match first.next.as_str() {
        "VERIFY_MOBILE" => Ok(first),
        // A TOTP-enrolled account: force the SMS path.
        "VERIFY_TOTP" => send_phone_otp(b, phone, Some(&first.temp_token), true).await,
        other => Err(AppError::Auth(format!(
            "Nubra asked for an unexpected login step ({}). Try again.",
            other
        ))),
    }
}

/// `POST /verifyphoneotp` (step 3): the SMS OTP for an `auth_token`.
pub async fn verify_phone_otp(
    b: &NubraBroker,
    phone: &str,
    otp: &str,
    temp_token: &str,
) -> Result<String> {
    let (_, v) = post(
        b,
        "/verifyphoneotp",
        &[
            ("x-temp-token", temp_token.to_string()),
            ("x-device-id", DEVICE_ID.to_string()),
        ],
        &json!({"phone": phone, "otp": otp.trim()}),
    )
    .await?;
    token(&v, "auth_token").ok_or_else(|| {
        AppError::Auth(format!(
            "Nubra did not accept the OTP: {}. Start the login again for a new OTP.",
            error_text(&v).unwrap_or_else(|| "verification failed".into())
        ))
    })
}

/// `Broker::authenticate`: the TOTP path.
pub async fn authenticate(b: &NubraBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let phone = creds.api_key.trim().to_string();
    let mpin = creds
        .api_secret
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if phone.is_empty() || mpin.is_empty() {
        return Err(AppError::Validation(
            "Save your Nubra registered mobile number as the API key and your MPIN as the API secret, then try again."
                .into(),
        ));
    }
    let totp = creds
        .totp
        .as_deref()
        .and_then(normalize_totp)
        .ok_or_else(|| {
            AppError::Validation("Enter the 6-digit TOTP from your authenticator app.".into())
        })?;
    let auth_token = totp_login(b, &phone, &totp).await?;
    let session = verify_pin(b, &auth_token, &mpin).await?;
    tracing::info!("Nubra sign-in completed");
    Ok(AuthResponse {
        auth_token: session,
        feed_token: None,
        user_id: phone,
        user_name: None,
    })
}
