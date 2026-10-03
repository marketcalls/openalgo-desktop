//! Samco Trade API v3.2 sign-in and the static IP diagnostic (web
//! `api/auth_api.py`, `blueprints/brlogin.py` `samco_ip_status`).
//!
//! `POST /session/token` with `{"apiKey", "apiSecret"}` (sent verbatim)
//! returns `{"status":"Success","sessionToken",..,"accountID","srcIp",
//! "primaryIp","secondaryIp"}`. The legacy OTP / password / IP-registration
//! endpoints are deprecated in v3.2; static IPs are managed in the Samco
//! dashboard. `GET /ip/whoami` reports the source IP Samco sees.

use super::mapping::text;
use super::{is_success, SamcoBroker};
use crate::brokers::types::AuthToken;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use reqwest::StatusCode;
use serde_json::{json, Value};

/// Samco Web Dashboard (API keys, secrets, static IPs).
pub const DASHBOARD_URL: &str = "https://tradeapi.samco.in/app/login";

/// web `/samco/ip-status` answer when nobody is signed in (HTTP 401).
pub const IP_STATUS_NOT_LOGGED_IN: &str = "Not logged in";
/// web `/samco/ip-status` answer without a Samco session (HTTP 400).
pub const IP_STATUS_NOT_CONNECTED: &str = "Not connected to Samco. Log in to the broker first.";

/// The fix for each `/session/token` error code (web `ERROR_CODE_HELP`,
/// worded for the desktop: credentials live in Profile, not `.env`).
pub fn error_code_help(code: &str) -> Option<String> {
    match code {
        "EOAUTH001" => Some(format!(
            "Check that the API key in Profile, Broker Configuration matches the API Key emailed when you created the Samco OAuth app, and that the app is Active at {}",
            DASHBOARD_URL
        )),
        "EOAUTH008" => Some(
            "Copy the secret again from the Samco dashboard (API Keys, Reveal Secret), or regenerate it, and update the API secret in Profile, Broker Configuration"
                .to_string(),
        ),
        "EOAUTH009" => Some(format!(
            "Register this computer's IP address under Static IPs at {}",
            DASHBOARD_URL
        )),
        _ => None,
    }
}

/// web `_error_message`: Samco's statusMessage plus the fix for its code.
pub fn error_message(v: &Value, default: &str) -> String {
    let msg = text(v.get("statusMessage"));
    let msg = if msg.is_empty() {
        default.to_string()
    } else {
        msg
    };
    match error_code_help(&text(v.get("errorCode"))) {
        Some(hint) => format!("{}. {}", msg, hint),
        None => msg,
    }
}

/// web `_log_ip_check`: compare Samco's view of our IP with the registered
/// static IPs. Order APIs reject an unregistered IP with HTTP 403.
pub fn ip_check(v: &Value) -> Option<&'static str> {
    let src = text(v.get("srcIp"));
    let primary = text(v.get("primaryIp"));
    let secondary = text(v.get("secondaryIp"));
    if src.is_empty() {
        return None;
    }
    if src == primary || src == secondary {
        tracing::info!("Samco source IP matches a registered static IP");
        Some("match")
    } else if primary.is_empty() && secondary.is_empty() {
        tracing::warn!(
            "Samco reports no registered static IP; order APIs will reject this computer until it is registered at {}",
            DASHBOARD_URL
        );
        Some("unregistered")
    } else {
        tracing::warn!(
            "Samco source IP does not match the registered static IPs; order APIs will reject this computer"
        );
        Some("mismatch")
    }
}

fn parse_body(step: &str, status: StatusCode, bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|_| {
        tracing::warn!(
            status = status.as_u16(),
            "Samco {} returned a response that is not JSON",
            step
        );
        json!({
            "status": "Failure",
            "statusMessage": "Samco did not answer normally. Try again shortly",
        })
    })
}

pub async fn authenticate(b: &SamcoBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let api_key = creds.api_key.trim().to_string();
    if api_key.is_empty() {
        return Err(AppError::Validation(format!(
            "Your Samco API key is missing. Enter the API Key of your Samco OAuth app (create one at {}) in Profile, Broker Configuration.",
            DASHBOARD_URL
        )));
    }
    let api_secret = creds
        .api_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Your Samco API secret is missing. Enter the API Secret shown when the OAuth app was created (see {}) in Profile, Broker Configuration.",
                DASHBOARD_URL
            ))
        })?
        .to_string();
    let resp = b
        .http
        .post(format!("{}/session/token", b.base_url))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(json!({"apiKey": api_key, "apiSecret": api_secret}).to_string())
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let v = parse_body("session/token", status, &bytes);
    let token = text(v.get("sessionToken"));
    if !is_success(&v) || token.is_empty() {
        tracing::warn!(
            error_code = %text(v.get("errorCode")),
            "Samco session token generation was refused"
        );
        return Err(AppError::Auth(format!(
            "Samco did not sign you in: {}",
            error_message(&v, "Failed to generate session token")
        )));
    }
    ip_check(&v);
    let account = text(v.get("accountID"));
    Ok(AuthResponse {
        auth_token: token,
        feed_token: None,
        user_id: if account.is_empty() {
            "samco".to_string()
        } else {
            account
        },
        user_name: None,
    })
}

/// `GET /ip/whoami` result.
#[derive(Debug, Clone, PartialEq)]
pub struct IpStatus {
    pub src_ip: String,
    pub primary_ip: String,
    pub secondary_ip: String,
    pub matches: bool,
    /// Samco's `matchedAs` (`primary`, `secondary` or null), verbatim.
    pub matched_as: Value,
    pub message: String,
}

impl IpStatus {
    pub fn from_whoami(v: &Value) -> Self {
        let truthy = |x: Option<&Value>| match x {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
            _ => false,
        };
        Self {
            src_ip: text(v.get("srcIp")),
            primary_ip: text(v.get("primaryIp")),
            secondary_ip: text(v.get("secondaryIp")),
            matches: truthy(v.get("matches")),
            matched_as: v.get("matchedAs").cloned().unwrap_or(Value::Null),
            message: text(v.get("statusMessage")),
        }
    }

    /// The web route's success body (HTTP 200), field for field.
    pub fn to_json(&self) -> Value {
        json!({
            "status": "success",
            "src_ip": self.src_ip,
            "primary_ip": self.primary_ip,
            "secondary_ip": self.secondary_ip,
            "matches": self.matches,
            "matched_as": self.matched_as,
            "message": self.message,
            "dashboard_url": DASHBOARD_URL,
        })
    }
}

/// The web route's error answer: `(http status, {"status":"error","message"})`.
/// 401 for [`IP_STATUS_NOT_LOGGED_IN`], 400 otherwise (not connected, or
/// the whoami call failed).
pub fn ip_status_error(message: &str) -> (u16, Value) {
    let code = if message == IP_STATUS_NOT_LOGGED_IN {
        401
    } else {
        400
    };
    (code, json!({"status": "error", "message": message}))
}

pub async fn ip_status(b: &SamcoBroker, auth: &AuthToken) -> Result<IpStatus> {
    let token = auth.raw().trim();
    if token.is_empty() {
        return Err(AppError::Auth(IP_STATUS_NOT_CONNECTED.into()));
    }
    let resp = b
        .http
        .get(format!("{}/ip/whoami", b.base_url))
        .header("Accept", "application/json")
        .header("x-session-token", token)
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let v = parse_body("ip/whoami", status, &bytes);
    if is_success(&v) {
        Ok(IpStatus::from_whoami(&v))
    } else {
        Err(AppError::Broker(error_message(
            &v,
            "Failed to fetch IP status",
        )))
    }
}
