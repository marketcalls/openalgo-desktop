//! Interactive and market-data logins (web `api/auth_api.py`,
//! `blueprints/brlogin.py` for the compositedge / rmoney callbacks).

use super::mapping;
use super::{MarketKeys, XtsBroker, XtsLogin};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use crate::security::Secret;
use serde_json::{json, Value};

/// A market-data session.
#[derive(Debug, Clone)]
pub(crate) struct MarketSession {
    pub token: String,
    pub user_id: String,
}

/// `POST <url> {secretKey, appKey[, source]}` -> `result.{token,userID}`
/// (`auth_api.py:61-100`, socket client `marketdata_login`).
pub(crate) async fn market_login_with(
    client: &reqwest::Client,
    broker: &'static str,
    url: &str,
    keys: &MarketKeys,
    source: bool,
) -> Result<MarketSession> {
    let mut body = json!({
        "secretKey": keys.secret.expose(),
        "appKey": keys.key.expose(),
    });
    if source {
        body["source"] = json!("WebAPI");
    }
    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let (status, v): (_, Value) = http::read_json(broker, resp).await?;
    if v.get("type").and_then(Value::as_str) == Some("success") {
        let r = v.get("result").cloned().unwrap_or(Value::Null);
        let token = mapping::s(&r, "token");
        if token.is_empty() {
            return Err(AppError::Auth(
                "The broker accepted the market data keys but sent no session. Try again.".into(),
            ));
        }
        return Ok(MarketSession {
            token,
            user_id: mapping::s(&r, "userID"),
        });
    }
    tracing::warn!(
        broker,
        status = status.as_u16(),
        "Market data login refused: {}",
        mapping::error_text(&v)
    );
    Err(AppError::Auth(
        "The broker refused the market data API key and secret. Check them on the Broker Configuration page."
            .into(),
    ))
}

pub(crate) async fn market_login(
    b: &XtsBroker,
    keys: &MarketKeys,
    url: &str,
    source: bool,
) -> Result<MarketSession> {
    market_login_with(&b.http, b.cfg.id, url, keys, source).await
}

/// The callback `session` value: JSON (possibly double encoded, like the
/// web's `json.loads` twice), else a bare token string.
pub fn parse_session(raw: &str) -> Value {
    let raw = raw.trim();
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::String(inner)) => {
            serde_json::from_str::<Value>(&inner).unwrap_or(Value::String(inner))
        }
        Ok(v) => v,
        Err(_) => Value::String(raw.to_string()),
    }
}

fn session_field(session: &Value, key: &str) -> Option<String> {
    match session {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(_) => Some(mapping::s(session, key)).filter(|s| !s.is_empty()),
        _ => None,
    }
}

/// The account a posted session token belongs to, as the broker names it
/// (web rmoney `session_account`, marketcalls/openalgo#2337). The session
/// arrives from the broker's site with a `userID` beside the token, which
/// is only a label anyone posting the form could write; the token is asked
/// for its own profile (`GET /user/profile`, authorised by the token), which
/// both checks it with the broker and names its account. A token the broker
/// refuses, or a profile that names no account, signs in nothing.
async fn confirmed_account(b: &XtsBroker, token: &str) -> Result<String> {
    let resp = b
        .http
        .get(b.interactive_url("/user/profile"))
        .header("authorization", token)
        .header("Content-Type", "application/json")
        .send()
        .await;
    let named = match resp {
        Ok(r) if r.status().is_success() => match http::read_json::<Value>(b.cfg.id, r).await {
            Ok((_, v)) => profile_account(&v),
            Err(e) => {
                tracing::warn!(
                    broker = b.cfg.id,
                    "Session profile unreadable: {}",
                    e.code()
                );
                None
            }
        },
        Ok(r) => {
            tracing::warn!(
                broker = b.cfg.id,
                status = r.status().as_u16(),
                "Session profile refused"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                broker = b.cfg.id,
                "Session profile request failed: {}",
                crate::brokers::common::redact::url_safe_error(&e)
            );
            None
        }
    };
    named.ok_or_else(|| {
        tracing::warn!(
            broker = b.cfg.id,
            "Sign-in refused: the broker did not confirm the session's account"
        );
        AppError::Auth(format!(
            "{} did not confirm which account this login is for, so it was not stored. Log in again.",
            b.cfg.name
        ))
    })
}

/// The account a `/user/profile` answer names (`result.ClientId`, in any of
/// the casings XTS servers use), or `None`.
fn profile_account(v: &Value) -> Option<String> {
    if v.get("type").and_then(Value::as_str) != Some("success") {
        return None;
    }
    let result = v.get("result").filter(|r| r.is_object())?;
    ["ClientId", "ClientID", "clientId", "clientID"]
        .iter()
        .map(|k| mapping::s(result, k))
        .find(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_string())
}

/// Interactive token and user id.
async fn interactive_session(b: &XtsBroker, creds: &BrokerCredentials) -> Result<(String, String)> {
    let cfg = b.cfg;
    let callback = creds
        .request_token
        .as_deref()
        .or(creds.auth_code.as_deref())
        .filter(|s| !s.trim().is_empty());
    let secret = creds.api_secret.clone().unwrap_or_default();
    let body = match cfg.login {
        XtsLogin::Direct => json!({
            "appKey": creds.api_key,
            "secretKey": secret,
            "source": "WebAPI",
        }),
        XtsLogin::DirectAccessToken => json!({
            "appKey": creds.api_key,
            "secretKey": secret,
            "accessToken": cfg.id,
        }),
        XtsLogin::OAuthAccessToken => {
            let session = parse_session(callback.ok_or_else(|| {
                AppError::Auth(format!(
                    "{} did not complete the sign-in. Try again.",
                    cfg.name
                ))
            })?);
            let access = session_field(&session, "accessToken").ok_or_else(|| {
                AppError::Auth(format!(
                    "{} did not send an access token. Try the sign-in again.",
                    cfg.name
                ))
            })?;
            json!({"appKey": creds.api_key, "secretKey": secret, "accessToken": access})
        }
        XtsLogin::OAuthSessionToken => {
            let session = parse_session(callback.ok_or_else(|| {
                AppError::Auth(format!(
                    "{} did not complete the sign-in. Try again.",
                    cfg.name
                ))
            })?);
            let token = session_field(&session, "token").ok_or_else(|| {
                AppError::Auth(format!(
                    "{} did not send a session token. Try the sign-in again.",
                    cfg.name
                ))
            })?;
            let user = confirmed_account(b, &token).await?;
            return Ok((token, user));
        }
    };
    let resp = b
        .http
        .post(b.interactive_url("/user/session"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let (status, v): (_, Value) = http::read_json(cfg.id, resp).await?;
    if v.get("type").and_then(Value::as_str) == Some("success") {
        let r = v.get("result").cloned().unwrap_or(Value::Null);
        let token = mapping::s(&r, "token");
        if token.is_empty() {
            return Err(AppError::Auth(format!(
                "{} signed in but returned no session. Try again.",
                cfg.name
            )));
        }
        return Ok((token, mapping::s(&r, "userID")));
    }
    let text = mapping::error_text(&v);
    tracing::warn!(
        broker = cfg.id,
        status = status.as_u16(),
        "Interactive login refused: {}",
        text
    );
    Err(AppError::Auth(if text.is_empty() {
        format!(
            "{} refused the sign-in. Check the interactive API key and secret.",
            cfg.name
        )
    } else {
        text
    }))
}

/// web `authenticate_broker` -> `(token, feed_token, user_id)`.
pub(crate) async fn authenticate(b: &XtsBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let needs_key = !matches!(b.cfg.login, XtsLogin::OAuthSessionToken);
    if needs_key && (creds.api_key.trim().is_empty() || creds.api_secret.is_none()) {
        return Err(AppError::Validation(
            "Add your interactive API key and secret on the Broker Configuration page, then try again."
                .into(),
        ));
    }
    let (token, interactive_user) = interactive_session(b, &creds).await?;
    let keys = match (&creds.api_key_market, &creds.api_secret_market) {
        (Some(k), Some(s)) if !k.trim().is_empty() && !s.is_empty() => Some(MarketKeys {
            key: Secret::new(k.trim()),
            secret: Secret::new(s.clone()),
        }),
        _ => None,
    };
    b.remember_market_keys(keys.clone());
    let market = match &keys {
        Some(k) => match market_login(b, k, &b.md_url("/auth/login"), true).await {
            Ok(m) => Some(m),
            Err(e) => {
                // Like the web: the trading session stands without a feed.
                tracing::warn!(broker = b.cfg.id, "Market data login failed: {}", e.code());
                None
            }
        },
        None => {
            tracing::warn!(
                broker = b.cfg.id,
                "No market data API key saved; quotes and the live feed need it"
            );
            None
        }
    };
    if let Some(m) = &market {
        refuse_another_account(b, &interactive_user, &m.user_id)?;
    }
    let user_id = market
        .as_ref()
        .map(|m| m.user_id.clone())
        .filter(|u| !u.is_empty())
        .unwrap_or(interactive_user);
    Ok(AuthResponse {
        auth_token: token,
        feed_token: market.map(|m| m.token),
        user_id,
        user_name: None,
    })
}

/// A third-party sign-in whose session names another account than the
/// market data keys is refused (web `brlogin.py` `_xts_account_mismatch`,
/// marketcalls/openalgo#2337). Both are the trader's own keys, so two
/// different accounts mean the session that came back is not the trader's.
/// Only the OAuth members are compared: a direct login makes its session
/// with the trader's own interactive keys. A side that names no account is
/// not a mismatch (the web leaves the case where the market data login
/// failed as it was).
fn refuse_another_account(b: &XtsBroker, session_user: &str, market_user: &str) -> Result<()> {
    if !matches!(
        b.cfg.login,
        XtsLogin::OAuthAccessToken | XtsLogin::OAuthSessionToken
    ) {
        return Ok(());
    }
    let (a, m) = (session_user.trim(), market_user.trim());
    if a.is_empty() || m.is_empty() || a.eq_ignore_ascii_case(m) {
        return Ok(());
    }
    tracing::warn!(
        broker = b.cfg.id,
        "Sign-in refused: the session and the market data keys name different accounts"
    );
    Err(AppError::Auth(
        "This login is for a different account than the market data keys saved for this broker. Log in with that account, or check the keys under Broker Configuration."
            .into(),
    ))
}
