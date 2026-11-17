//! Nuvama sign-in (web `api/auth_api.py`, the SDK's `login_helper.py`).
//!
//! The trader logs in on Nuvama's page
//! (`https://www.nuvamawealth.com/api-connect/login?api_key=...`), which
//! returns to the registered redirect URL with `request_id`. Two calls then
//! make the session:
//!
//! 1. `POST {login}accounts/loginvendor/{api_key}/` with `{"pwd": secret}`
//!    answers `msg`, the vendor session sent as `SourceToken` from then on.
//! 2. `POST {login}accounts/logindata/` with `{"reqId": request_id}`
//!    answers `data.auth` (the `Authorization` value) and the account in
//!    `data.lgnData.accs`; the `AppIdKey` response header is the app id key
//!    this session uses.

use super::client::has_error;
use super::session::{initial_app_id_key, json_headers, NuvamaSession};
use super::NuvamaBroker;
use crate::brokers::common::redact;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use crate::security::Secret;
use serde_json::{json, Value};
use std::time::Duration;

/// web `timeout=20.0` on both login calls.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);

fn refused(message: &str) -> AppError {
    AppError::Auth(message.into())
}

/// Nuvama's error text in a login answer (web `_error_text`).
fn error_text(body: &Value) -> String {
    if let Some(m) = body
        .get("error")
        .and_then(|e| e.get("errMsg"))
        .and_then(Value::as_str)
    {
        return m.to_string();
    }
    let failed = body.get("success").and_then(Value::as_bool) == Some(false);
    match body.get("msg").and_then(Value::as_str) {
        Some(m) if failed => m.to_string(),
        _ => "no session in the answer".into(),
    }
}

fn header(resp: &reqwest::Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

async fn body_of(resp: reqwest::Response) -> Value {
    resp.json::<Value>().await.unwrap_or(Value::Null)
}

/// Step 1: the vendor session for this API key, and the app id key to go
/// on with.
async fn vendor_session(
    b: &NuvamaBroker,
    api_key: &str,
    api_secret: &str,
) -> Result<(Secret, String)> {
    let initial = initial_app_id_key();
    let url = format!(
        "{}accounts/loginvendor/{}/",
        b.urls.login,
        urlencoding::encode(api_key)
    );
    let resp = json_headers(b.http.post(url))
        .header("AppIdKey", &initial)
        .header("Source", api_key)
        .timeout(LOGIN_TIMEOUT)
        .body(json!({ "pwd": api_secret }).to_string())
        .send()
        .await
        .map_err(redact::http)?;
    let status = resp.status().as_u16();
    let app_id_key = header(&resp, "AppIdKey").unwrap_or(initial);
    let body = body_of(resp).await;
    let source_token = body
        .get("msg")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty());
    match source_token {
        Some(t) if status == 200 && !has_error(&body) => Ok((Secret::new(t), app_id_key)),
        _ => {
            tracing::error!(
                status,
                "Nuvama vendor login failed: {}",
                redact::url_safe(&error_text(&body))
            );
            Err(refused(
                "Nuvama did not accept the API key and secret. Check them in Broker Configuration.",
            ))
        }
    }
}

/// The account a login answer names (web: `eqAccID`, else `coAccID` for a
/// commodity-only account, whose `eqAccID` Nuvama sends as `""`) and its
/// type.
pub(super) fn account_of(data: &Value) -> Option<(String, String)> {
    let lgn = data.get("lgnData")?;
    let accs = lgn.get("accs")?;
    let account = ["eqAccID", "coAccID"]
        .iter()
        .find_map(|k| {
            accs.get(*k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|a| !a.is_empty())
        })?
        .to_string();
    let kind = lgn
        .get("accTyp")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    Some((account, kind))
}

pub async fn authenticate(b: &NuvamaBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let request_id = creds
        .request_token
        .as_deref()
        .or(creds.auth_code.as_deref())
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .ok_or_else(|| refused("Nuvama did not send back a request id. Start the login again."))?
        .to_string();
    let api_key = creds.api_key.trim().to_string();
    let api_secret = creds
        .api_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let Some(api_secret) = api_secret.filter(|_| !api_key.is_empty()) else {
        return Err(AppError::Validation(
            "Add your Nuvama API key and API secret in Broker Configuration.".into(),
        ));
    };

    let (source_token, app_id_key) = vendor_session(b, &api_key, &api_secret).await?;

    // Step 2: the SDK sends this call without the Source header.
    let resp = json_headers(b.http.post(format!("{}accounts/logindata/", b.urls.login)))
        .header("SourceToken", source_token.expose())
        .header("AppIdKey", &app_id_key)
        .timeout(LOGIN_TIMEOUT)
        .body(json!({ "reqId": request_id }).to_string())
        .send()
        .await
        .map_err(redact::http)?;
    let status = resp.status().as_u16();
    let app_id_key = header(&resp, "AppIdKey").unwrap_or(app_id_key);
    let body = body_of(resp).await;
    if status != 200 || !body.is_object() || has_error(&body) {
        tracing::error!(
            status,
            "Nuvama logindata failed: {}",
            redact::url_safe(&error_text(&body))
        );
        return Err(refused(if status == 222 {
            "The Nuvama login link expired before it was used. Log in again."
        } else {
            "Nuvama did not complete the login. Log in again."
        }));
    }
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    let auth = data
        .get("auth")
        .and_then(Value::as_str)
        .filter(|a| !a.is_empty());
    let (Some(auth), Some((account_id, account_type))) = (auth, account_of(&data)) else {
        tracing::error!("Nuvama logindata answered without a session or an account id");
        return Err(refused("Nuvama did not complete the login. Log in again."));
    };
    let session = NuvamaSession {
        auth: Secret::new(auth),
        source_token,
        app_id_key: Secret::new(app_id_key),
        account_id: account_id.clone(),
        account_type,
        api_key: Secret::new(api_key),
    };
    tracing::info!(
        "Nuvama login completed for account type {}",
        if session.account_type.is_empty() {
            "unknown"
        } else {
            &session.account_type
        }
    );
    Ok(AuthResponse {
        auth_token: session.pack(),
        feed_token: None,
        user_id: account_id,
        user_name: None,
    })
}
