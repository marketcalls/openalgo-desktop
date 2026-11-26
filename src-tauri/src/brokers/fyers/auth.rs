//! Fyers auth-code exchange (web `api/auth_api.py`) and access-token claims.

use super::{code_message, FyersBroker};
use crate::brokers::common::{http, jwt};
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// `sha256("app_id:app_secret")` as hex (web `appIdHash`).
pub fn app_id_hash(api_key: &str, api_secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("{}:{}", api_key, api_secret).as_bytes());
    hex::encode(h.finalize())
}

/// The claims of a Fyers access token this adapter reads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct JwtClaims {
    /// Key for the HSM market-data socket.
    pub hsm_key: Option<String>,
    /// Expiry, epoch seconds.
    pub exp: i64,
    /// Fyers client id.
    pub fy_id: Option<String>,
}

/// Decode the payload of a Fyers JWT (`app_id:` prefix tolerated), without
/// verifying the signature (web `_extract_hsm_key`).
pub fn decode_jwt_claims(token: &str) -> Option<JwtClaims> {
    let token = token.split_once(':').map(|(_, t)| t).unwrap_or(token);
    // A token has its three parts (`header.payload.signature`).
    token.split('.').nth(2)?;
    serde_json::from_value(Value::Object(jwt::claims(token)?)).ok()
}

pub async fn authenticate(b: &FyersBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let code = creds
        .auth_code
        .or(creds.request_token)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Fyers did not return a login code. Start the Fyers login again.".into(),
            )
        })?;
    let secret = creds.api_secret.filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Validation(
            "Your Fyers app secret is missing. Add it on the Broker Configuration page.".into(),
        )
    })?;
    if creds.api_key.is_empty() {
        return Err(AppError::Validation(
            "Your Fyers app id is missing. Add it on the Broker Configuration page.".into(),
        ));
    }
    let body = json!({
        "grant_type": "authorization_code",
        "appIdHash": app_id_hash(&creds.api_key, &secret),
        "code": code,
    });
    let resp = b
        .http
        .post(format!("{}/api/v3/validate-authcode", b.urls.api))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await?;
    let (status, v): (_, Value) = http::read_json("fyers", resp).await?;
    let token = v
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty());
    let token = match (super::is_ok(&v), token) {
        (true, Some(t)) => t.to_string(),
        (true, None) => {
            tracing::warn!("Fyers login answered without an access token");
            return Err(AppError::Auth(
                "Fyers accepted the login but returned no session. Log in to Fyers again.".into(),
            ));
        }
        (false, _) => {
            let (code, message) = code_message(&v);
            tracing::warn!(status = status.as_u16(), code, "Fyers login refused");
            return Err(AppError::Auth(if message.is_empty() {
                "Fyers did not accept the login. Start the Fyers login again.".into()
            } else {
                format!("Fyers did not accept the login: {}", message)
            }));
        }
    };
    let claims = decode_jwt_claims(&token);
    let user_id = claims
        .as_ref()
        .and_then(|c| c.fy_id.clone())
        .filter(|s| !s.is_empty())
        .or(creds.client_id.filter(|s| !s.is_empty()))
        .unwrap_or_else(|| creds.api_key.split('-').next().unwrap_or("").to_string());
    Ok(AuthResponse {
        // web stores the access token and prefixes the app id per call; the
        // desktop stores the prefixed pair once.
        auth_token: format!("{}:{}", creds.api_key, token),
        // The HSM key is read from the access token's claims at connect time.
        feed_token: None,
        user_id,
        user_name: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn app_id_hash_is_sha256_of_pair() {
        // sha256("APPID-100:secret")
        let mut h = Sha256::new();
        h.update(b"APPID-100:secret");
        assert_eq!(
            app_id_hash("APPID-100", "secret"),
            hex::encode(h.finalize())
        );
    }

    #[test]
    fn jwt_claims_decode_with_and_without_prefix() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"hsm_key":"abc123","exp":4102444800,"fy_id":"<USER_ID>"}"#);
        let jwt = format!("eyJhbGciOiJIUzI1NiJ9.{}.sig", payload);
        let c = decode_jwt_claims(&jwt).unwrap();
        assert_eq!(c.hsm_key.as_deref(), Some("abc123"));
        assert_eq!(c.exp, 4102444800);
        let c = decode_jwt_claims(&format!("APPID-100:{}", jwt)).unwrap();
        assert_eq!(c.fy_id.as_deref(), Some("<USER_ID>"));
        assert!(decode_jwt_claims("not-a-jwt").is_none());
        assert!(decode_jwt_claims("a.!!!.c").is_none());
    }
}
