//! The plan's credential: the auth record the device flow produces (the
//! web's LiteLLM `Authenticator._build_auth_record`), kept in the vault
//! under `oauth:chatgpt` as canonical JSON, and nowhere else (no token
//! file: decision 10 of the 1.0.4 plan).
//!
//! The record is `{access_token, refresh_token, id_token, expires_at,
//! account_id}`. `expires_at` is the access token's own `exp` claim and
//! `account_id` the `chatgpt_account_id` of the id token (else of the access
//! token), both read by a local decode, never a network call. The JSON is
//! written with sorted keys and no spaces (the web's `_canonical`), so an
//! unchanged credential compares equal and the vault skips the rewrite.

use crate::agent::models::secrets;
use crate::security::{Secret, SecurityManager};
use base64::Engine;
use rusqlite::Connection;
use serde_json::{Map, Value};

/// The vault row (the web's `SECRET_NAME`).
pub const SECRET_NAME: &str = "oauth:chatgpt";

/// The claim the account id lives under (LiteLLM's `OPENAI_AUTH_CLAIM_KEY`).
const AUTH_CLAIM: &str = "https://api.openai.com/auth";

/// The plan's sign-in, as stored.
#[derive(Default)]
pub struct AuthRecord {
    pub access_token: Option<Secret>,
    pub refresh_token: Option<Secret>,
    pub id_token: Option<Secret>,
    /// Unix seconds the access token stops being accepted.
    pub expires_at: Option<f64>,
    pub account_id: Option<String>,
}

impl std::fmt::Debug for AuthRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthRecord")
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// A token's claims, decoded without verifying it (LiteLLM's
/// `_decode_jwt_claims`): empty for anything that is not a JWT.
pub fn claims(token: &str) -> Map<String, Value> {
    let Some(payload) = token.split('.').nth(1) else {
        return Map::new();
    };
    let Ok(bytes) =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('='))
    else {
        return Map::new();
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// The `exp` claim (LiteLLM's `_get_expires_at`: a number, truncated).
pub fn expiry_claim(token: &str) -> Option<f64> {
    claims(token)
        .get("exp")
        .and_then(Value::as_f64)
        .map(f64::trunc)
}

/// The account id inside a token (LiteLLM's `_extract_account_id`).
pub fn account_claim(token: &str) -> Option<String> {
    claims(token)
        .get(AUTH_CLAIM)
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn present(s: &Option<Secret>) -> Option<&str> {
    s.as_ref().map(|s| s.expose()).filter(|s| !s.is_empty())
}

impl AuthRecord {
    /// The record for freshly exchanged tokens (the web's
    /// `_build_auth_record`).
    pub fn from_tokens(access: Secret, refresh: Option<Secret>, id: Secret) -> AuthRecord {
        let expires_at = expiry_claim(access.expose());
        let account_id = account_claim(id.expose()).or_else(|| account_claim(access.expose()));
        AuthRecord {
            access_token: Some(access),
            refresh_token: refresh.filter(|r| !r.is_empty()),
            id_token: Some(id),
            expires_at,
            account_id,
        }
    }

    pub fn access(&self) -> Option<&str> {
        present(&self.access_token)
    }

    pub fn refresh(&self) -> Option<&str> {
        present(&self.refresh_token)
    }

    /// When the access token expires: the stored time, else its own claim;
    /// `None` when neither says.
    pub fn expiry(&self) -> Option<f64> {
        self.expires_at
            .or_else(|| self.access().and_then(expiry_claim))
    }

    /// Whether the access token is unusable `skew` seconds from `now`. An
    /// expiry that cannot be established counts as expired. The web's own
    /// check uses no skew (`_access_token_expired`); a renewal uses
    /// [`super::refresh::SKEW_SECONDS`] (D-025).
    pub fn access_expired(&self, now: f64, skew: f64) -> bool {
        match self.expiry() {
            Some(at) => now >= at - skew,
            None => true,
        }
    }

    /// Whether the record can still authenticate a call (the web's
    /// `_has_credential`): a refresh token alone is enough, else an access
    /// token that has not expired.
    pub fn has_credential(&self, now: f64) -> bool {
        self.refresh().is_some() || (self.access().is_some() && !self.access_expired(now, 0.0))
    }

    /// The identifier shown for the credential: over the refresh token (it
    /// survives a renewal), else the access token. Never the vault row's
    /// own fingerprint, which is over the whole JSON.
    pub fn fingerprint(&self) -> String {
        match self.refresh().or(self.access()) {
            Some(t) => secrets::fingerprint(t),
            None => "...????".into(),
        }
    }

    /// The canonical JSON (sorted keys, compact), as the vault stores it.
    pub fn canonical(&self) -> Secret {
        let text = |s: &Option<Secret>| -> Value {
            s.as_ref()
                .map(|s| Value::String(s.expose().to_string()))
                .unwrap_or(Value::Null)
        };
        let expires = match self.expires_at {
            Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Value::from(f as i64),
            Some(f) => serde_json::Number::from_f64(f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            None => Value::Null,
        };
        // Sorted by key, as Python's `sort_keys=True` writes them.
        let mut m = Map::new();
        m.insert("access_token".into(), text(&self.access_token));
        m.insert(
            "account_id".into(),
            self.account_id
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        m.insert("expires_at".into(), expires);
        m.insert("id_token".into(), text(&self.id_token));
        m.insert("refresh_token".into(), text(&self.refresh_token));
        Secret::new(Value::Object(m).to_string())
    }

    /// A stored record, read leniently: a field of the wrong type is absent.
    pub fn parse(json: &str) -> Option<AuthRecord> {
        let Ok(Value::Object(m)) = serde_json::from_str::<Value>(json) else {
            return None;
        };
        let text = |k: &str| {
            m.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(Secret::new)
        };
        Some(AuthRecord {
            access_token: text("access_token"),
            refresh_token: text("refresh_token"),
            id_token: text("id_token"),
            expires_at: m.get("expires_at").and_then(Value::as_f64),
            account_id: m
                .get("account_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        })
    }
}

/// The stored sign-in, or `None` (none stored, the keys are locked, or the
/// row does not hold a record). Errors are logged by kind, never quoted.
pub fn load(conn: &Connection, security: &SecurityManager) -> Option<AuthRecord> {
    let stored = secrets::get(conn, security, SECRET_NAME)?;
    let record = AuthRecord::parse(stored.expose());
    if record.is_none() {
        tracing::error!("The stored ChatGPT subscription sign-in is not a record");
    }
    record
}

/// Store the sign-in and read it back: a write that did not land is a
/// failure, never a reported success (the web's `_write_record` check).
pub fn store(
    conn: &Connection,
    security: &SecurityManager,
    record: &AuthRecord,
) -> Result<(), &'static str> {
    let text = record.canonical();
    secrets::set(conn, security, SECRET_NAME, text.expose())?;
    match secrets::get(conn, security, SECRET_NAME) {
        Some(back) if back.expose() == text.expose() => Ok(()),
        _ => {
            tracing::error!(
                "The ChatGPT subscription sign-in did not read back after it was stored"
            );
            Err("Could not store the secret")
        }
    }
}

/// Whether a sign-in row exists (the status's `stored_in_database`); no
/// decryption.
pub fn is_stored(conn: &Connection) -> bool {
    secrets::describe(conn, &[SECRET_NAME.to_string()])
        .map(|m| m.get(SECRET_NAME).is_some_and(|s| s.has_value))
        .unwrap_or(false)
}

/// Remove the sign-in; true when a row went.
pub fn delete(conn: &Connection) -> crate::error::Result<bool> {
    secrets::delete(conn, SECRET_NAME)
}
