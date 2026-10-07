//! Definedge OTP sign-in (web `api/auth_api.py`, `blueprints/brlogin.py`
//! lines 820-911).
//!
//! Step 1, send OTP: `GET <signin>/login/<api_token>` with header
//! `api_secret: <secret>` -> `{"otp_token", "message"}`. The web runs it
//! when the login page opens (and on "resend"); the `otp_token` waits in one
//! slot on the broker for at most `OTP_TTL`.
//!
//! Step 2, verify: `POST <signin>/token` with
//! `{"otp_token", "otp", "ac": sha256(otp_token + otp + api_secret)}` ->
//! `{"stat":"Ok","api_session_key","susertoken","uid"|"uccid"}`.

use super::{DefinedgeBroker, DefinedgeSession, PendingOtp, OTP_TTL};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use crate::security::Secret;
use reqwest::StatusCode;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Instant;

/// `ac` of step two: hex SHA-256 of `otp_token + otp + api_secret`.
pub fn auth_code(otp_token: &str, otp: &str, api_secret: &str) -> String {
    hex::encode(Sha256::digest(
        format!("{}{}{}", otp_token, otp, api_secret).as_bytes(),
    ))
}

fn creds(c: &BrokerCredentials) -> Result<(String, String)> {
    let token = c.api_key.trim().to_string();
    if token.is_empty() {
        return Err(AppError::Validation(
            "Your Definedge API token is missing. Enter it as the API key in Profile, Broker Configuration."
                .into(),
        ));
    }
    let secret = c
        .api_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            AppError::Validation(
                "Your Definedge API secret is missing. Enter it as the API secret in Profile, Broker Configuration."
                    .into(),
            )
        })?;
    Ok((token, secret))
}

/// Step 1 (web `login_step1`): ask Definedge to send an OTP. Stores the
/// returned `otp_token` (replacing any earlier one) and returns the
/// trader-facing message.
pub async fn send_otp(b: &DefinedgeBroker, c: &BrokerCredentials) -> Result<String> {
    let (api_token, api_secret) = creds(c)?;
    let url = format!(
        "{}/login/{}",
        b.urls.signin,
        urlencoding::encode(&api_token)
    );
    let resp = b
        .http
        .get(url)
        .header("api_secret", api_secret)
        .send()
        .await?;
    let status = resp.status();
    let refused = || {
        AppError::Auth(
            "Definedge did not send an OTP. Check the API token and secret in Profile, Broker Configuration, then try again."
                .into(),
        )
    };
    if !status.is_success() {
        tracing::warn!(
            broker = "definedge",
            status = status.as_u16(),
            "OTP request refused"
        );
        return Err(refused());
    }
    let (_, v): (StatusCode, Value) = http::read_json("definedge", resp).await?;
    let token = super::text(&v, "otp_token");
    if token.is_empty() {
        tracing::warn!(broker = "definedge", "OTP response carried no otp_token");
        return Err(refused());
    }
    *b.pending_otp.lock() = Some(PendingOtp {
        token: Secret::new(token),
        sent_at: Instant::now(),
    });
    let msg = super::text(&v, "message");
    Ok(if msg.is_empty() {
        "OTP has been sent successfully".to_string()
    } else {
        msg
    })
}

/// Step 2 (web `login_step2` + `authenticate_broker`).
pub async fn verify_otp(
    b: &DefinedgeBroker,
    api_token: &str,
    api_secret: &str,
    otp_token: &str,
    otp: &str,
) -> Result<AuthResponse> {
    let body = json!({
        "otp_token": otp_token,
        "otp": otp,
        "ac": auth_code(otp_token, otp, api_secret),
    });
    let resp = b
        .http
        .post(format!("{}/token", b.urls.signin))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !status.is_success() || super::text(&v, "stat") != "Ok" {
        tracing::warn!(
            broker = "definedge",
            status = status.as_u16(),
            "OTP verification refused"
        );
        let why = super::text(&v, "emsg");
        return Err(AppError::Auth(if why.is_empty() {
            "Definedge did not accept the OTP. Check the code and try again, or request a new OTP."
                .to_string()
        } else {
            format!(
                "Definedge did not accept the OTP: {}. Check the code and try again, or request a new OTP.",
                why
            )
        }));
    }
    let session_key = super::text(&v, "api_session_key");
    if session_key.is_empty() {
        return Err(AppError::Auth(
            "Definedge accepted the OTP but returned no session. Log in again.".into(),
        ));
    }
    let susertoken = super::text(&v, "susertoken");
    let uid = Some(super::text(&v, "uid"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| super::text(&v, "uccid"));
    let session = DefinedgeSession {
        api_session_key: session_key,
        susertoken: susertoken.clone(),
        api_token: api_token.to_string(),
    };
    Ok(AuthResponse {
        auth_token: session.compose(),
        feed_token: Some(susertoken).filter(|s| !s.is_empty()),
        user_id: uid,
        user_name: None,
    })
}

/// The OTP comes in `credentials.totp` (the login form's `otp`). Without a
/// pending OTP token, step one runs now and the trader is asked to submit
/// the OTP that was just sent.
pub async fn authenticate(b: &DefinedgeBroker, c: BrokerCredentials) -> Result<AuthResponse> {
    let (api_token, api_secret) = creds(&c)?;
    let pending = {
        let mut slot = b.pending_otp.lock();
        if slot
            .as_ref()
            .is_some_and(|p| p.sent_at.elapsed() >= OTP_TTL)
        {
            *slot = None;
        }
        slot.as_ref().map(|p| p.token.expose().to_string())
    };
    let Some(otp_token) = pending else {
        send_otp(b, &c).await?;
        return Err(AppError::Auth(
            "We asked Definedge to send an OTP to your registered mobile/email. Enter it and submit again."
                .into(),
        ));
    };
    let otp = c.totp.as_deref().map(str::trim).unwrap_or_default();
    if otp.is_empty() {
        return Err(AppError::Validation(
            "Enter the OTP Definedge sent to your registered mobile/email.".into(),
        ));
    }
    let resp = verify_otp(b, &api_token, &api_secret, &otp_token, otp).await?;
    *b.pending_otp.lock() = None;
    Ok(resp)
}

impl DefinedgeBroker {
    /// Send (or resend) the login OTP; the route calls this when the login
    /// page opens and on "resend", like the web's GET `/definedge/callback`.
    pub async fn send_otp(&self, creds: &BrokerCredentials) -> Result<String> {
        send_otp(self, creds).await
    }
}
