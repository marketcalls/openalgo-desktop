//! HMAC-SHA256 request signing and the sign-in probe (web `api/baseurl.py`,
//! `api/auth_api.py`, `streaming/delta_websocket.py`).
//!
//! Every private call carries `api-key`, `timestamp` (epoch seconds) and
//! `signature = hex(HMAC_SHA256(secret, METHOD + timestamp + path +
//! query_string + body))`, where `query_string` keeps its leading `?` and is
//! built from the sorted parameters without URL-encoding, exactly as sent.
//! There is no session token: the stored session is `api_key:api_secret`
//! (encrypted at rest like every broker token) and each request is signed
//! afresh.

use super::DeltaBroker;
use crate::brokers::types::AuthToken;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

/// Hex HMAC-SHA256 of `message` under `secret` (web `generate_signature`).
pub fn hmac_hex(secret: &str, message: &str) -> String {
    // HMAC takes a key of any length; `new_from_slice` cannot fail for it.
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Request signature: `METHOD + timestamp + path + query_string + body`.
pub fn signature(
    secret: &str,
    method: &str,
    timestamp: &str,
    path: &str,
    query: &str,
    body: &str,
) -> String {
    let mut prehash = String::with_capacity(
        method.len() + timestamp.len() + path.len() + query.len() + body.len(),
    );
    prehash.push_str(&method.to_ascii_uppercase());
    prehash.push_str(timestamp);
    prehash.push_str(path);
    prehash.push_str(query);
    prehash.push_str(body);
    hmac_hex(secret, &prehash)
}

/// `?k=v&k2=v2` from parameters sorted by key, not URL-encoded (web
/// `"?" + "&".join(f"{k}={v}" for k, v in sorted(params.items()))`).
/// Empty when there are no parameters.
pub fn query_string(params: &[(&str, String)]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let mut p: Vec<&(&str, String)> = params.iter().collect();
    p.sort_by(|a, b| a.0.cmp(b.0));
    let joined: Vec<String> = p.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
    format!("?{}", joined.join("&"))
}

/// Private WebSocket `key-auth` frame: signature over `GET + ts + /live`.
pub fn ws_auth_message(api_key: &str, secret: &str, timestamp: &str) -> Value {
    json!({
        "type": "key-auth",
        "payload": {
            "api-key": api_key,
            "signature": hmac_hex(secret, &format!("GET{}/live", timestamp)),
            "timestamp": timestamp,
        }
    })
}

/// `(api_key, api_secret)` from the stored session.
pub fn credentials(auth: &AuthToken) -> Result<(&str, &str)> {
    auth.pair().ok_or_else(session_missing)
}

pub fn session_missing() -> AppError {
    AppError::Auth(
        "Your Delta Exchange API key is not available. Log in to Delta Exchange again from the broker page."
            .into(),
    )
}

/// The sign-in: a signed `GET /v2/profile` with the stored key and secret.
pub async fn authenticate(b: &DeltaBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let key = creds.api_key.trim().to_string();
    let secret = creds
        .api_secret
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if key.is_empty() || secret.is_empty() {
        return Err(AppError::Validation(
            "Enter your Delta Exchange API key and API secret on the broker page, then log in again."
                .into(),
        ));
    }
    if key.contains(':') {
        return Err(AppError::Validation(
            "The Delta Exchange API key looks wrong. Copy it again from the Delta Exchange API keys page."
                .into(),
        ));
    }
    let auth = AuthToken::new(format!("{}:{}", key, secret));
    let profile: Value = b
        .signed(&auth, reqwest::Method::GET, "/v2/profile", &[], None)
        .await?;
    let user_id = profile
        .get("id")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .filter(|s| !s.is_empty() && s != "null")
        .unwrap_or_else(|| "deltaexchange".to_string());
    let user_name = ["nick_name", "first_name"]
        .iter()
        .find_map(|k| {
            profile
                .get(*k)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
        })
        .map(str::to_string);
    tracing::info!("Delta Exchange API key verified");
    Ok(AuthResponse {
        auth_token: auth.raw().to_string(),
        feed_token: None,
        user_id,
        user_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-0123456789";

    /// Vectors produced by the web's own `generate_signature`
    /// (`broker/deltaexchange/api/baseurl.py`) with the same inputs.
    #[test]
    fn signatures_match_the_web_vectors() {
        let ts = "1791000000";
        assert_eq!(
            signature(SECRET, "GET", ts, "/v2/profile", "", ""),
            "bb6bb507c3386053888929ec28fac53d9fb7776c99740ccd5768b1a072ad6953"
        );
        assert_eq!(
            signature(SECRET, "get", ts, "/v2/orders", "?state=open", ""),
            "91b8e673258004431c17ab03d1fb2fdd37e4909524ce50fc9c9dc854cc19e022"
        );
        let body = r#"{"product_id": 27, "product_symbol": "BTCUSD", "size": 1, "side": "buy", "order_type": "limit_order", "time_in_force": "gtc", "limit_price": "60000"}"#;
        assert_eq!(
            signature(SECRET, "POST", ts, "/v2/orders", "", body),
            "3b6889bb9fc859f573e79b9bbe9b5a879a473d57920c84cef0806333faf1be0d"
        );
        assert_eq!(
            signature(
                SECRET,
                "DELETE",
                ts,
                "/v2/orders",
                "",
                r#"{"id":123,"product_id":27}"#
            ),
            "f9e907b30d0db116d204a1d6744871eaa303a93e66ae35a66388e67a21e4c03c"
        );
    }

    /// RFC 4231 test case 2 pins the HMAC primitive itself.
    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(
            hmac_hex("Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn websocket_auth_frame() {
        let m = ws_auth_message("key-1", SECRET, "1791000000");
        assert_eq!(m["type"], "key-auth");
        assert_eq!(m["payload"]["api-key"], "key-1");
        assert_eq!(m["payload"]["timestamp"], "1791000000");
        assert_eq!(
            m["payload"]["signature"],
            "b3768df2d16fcd2c1d3381a5a3ee0d9e16c2c6d7d84791429ee75fa42171e6a6"
        );
    }

    #[test]
    fn query_string_is_sorted_and_unencoded() {
        assert_eq!(query_string(&[]), "");
        assert_eq!(
            query_string(&[
                ("size", "1".into()),
                ("order_type", "limit_order".into()),
                ("side", "buy".into()),
                ("limit_price", "60000.0".into()),
            ]),
            "?limit_price=60000.0&order_type=limit_order&side=buy&size=1"
        );
        assert_eq!(
            query_string(&[("symbol", "C-BTC-62000-271126".into())]),
            "?symbol=C-BTC-62000-271126"
        );
    }

    #[test]
    fn credentials_come_from_the_stored_pair() {
        let a = AuthToken::new("key-1:secret-1");
        assert_eq!(credentials(&a).unwrap(), ("key-1", "secret-1"));
        assert!(credentials(&AuthToken::new("only-key")).is_err());
    }
}
