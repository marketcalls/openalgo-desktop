//! JSON envelopes and the `/api/v1` body extractor.
//!
//! Every failure answers in JSON. The exact shapes come from the golden
//! fixtures recorded against the web (`tests/fixtures/rest/errors`), quirks
//! included:
//!
//! | Situation                          | Status | Body                                                      |
//! |------------------------------------|--------|-----------------------------------------------------------|
//! | malformed or empty JSON body       | 400    | `{"message": "The browser (or proxy) sent a request..."}` (no `status`) |
//! | JSON body that is not an object    | 500    | `{"message": "Internal Server Error"}` (no `status`)      |
//! | non-JSON content type              | 500    | `{"status":"error","message":"An unexpected error occurred"}` |
//! | schema validation                  | 400    | `{"status":"error","message":{field:[msgs]}}`             |
//! | invalid API key                    | 403    | `{"status":"error","message":"Invalid openalgo apikey"}`  |
//! | unknown `/api/v1` route or method  | 404    | `{"status":"error","message":"Not found","path":...}`     |

use axum::{
    body::Bytes,
    extract::{FromRequest, Request},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub const BAD_REQUEST_MSG: &str =
    "The browser (or proxy) sent a request that this server could not understand.";
pub const UNEXPECTED: &str = "An unexpected error occurred";

/// The reply a request helper gives back when the request ends there (a
/// refusal or an error) instead of a value; the handler sends it as is
/// (`Err(r) => return *r`). Boxed: a `Response` is too large to travel in
/// every `Result` (clippy `result_large_err`), and `?` boxes one on the way.
pub type EarlyReply = Box<Response>;

pub fn json_response(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// `{"status":"error","message":msg}`
pub fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    json_response(status, json!({"status": "error", "message": msg.into()}))
}

/// The web's 404 body, with its path's webhook credential masked.
pub fn not_found(path: &str) -> Response {
    json_response(
        StatusCode::NOT_FOUND,
        json!({"status": "error", "message": "Not found", "path": redact_path(path)}),
    )
}

/// Webhook addresses whose next segment is the credential.
const CREDENTIAL_PREFIXES: [&str; 3] =
    ["/strategy/webhook/", "/flow/webhook/", "/chartink/webhook/"];

/// The web's `redact_url_credentials` on a request path: the segment after
/// a webhook prefix (any case) becomes `<redacted>`, so a 404 never echoes
/// a token (parity R-05).
pub fn redact_path(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    let mut out = String::with_capacity(path.len());
    let mut at = 0;
    while let Some((start, len)) = CREDENTIAL_PREFIXES
        .iter()
        .filter_map(|p| lower[at..].find(p).map(|i| (at + i, p.len())))
        .min()
    {
        let token = start + len;
        let end = path[token..]
            .find(|c: char| c.is_whitespace() || "/?#'\"<>".contains(c))
            .map_or(path.len(), |i| token + i);
        out.push_str(&path[at..token]);
        if end > token {
            out.push_str("<redacted>");
        }
        at = end;
    }
    out.push_str(&path[at..]);
    out
}

pub fn unexpected() -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, UNEXPECTED)
}

/// Field errors in marshmallow's shape.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FieldErrors(pub BTreeMap<String, Vec<String>>);

impl FieldErrors {
    pub fn add(&mut self, field: &str, msg: &str) {
        self.0
            .entry(field.to_string())
            .or_default()
            .push(msg.to_string());
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_response(self) -> Response {
        json_response(
            StatusCode::BAD_REQUEST,
            json!({"status": "error", "message": self.0}),
        )
    }
}

fn is_json(req: &Request) -> bool {
    req.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            let ct = ct.to_ascii_lowercase();
            ct.starts_with("application/json")
                || (ct.starts_with("application/") && ct.contains("+json"))
        })
        .unwrap_or(false)
}

/// Read the body as a JSON object with the web's error behaviour.
pub async fn read_json_object<S: Send + Sync>(
    req: Request,
    state: &S,
) -> Result<Map<String, Value>, EarlyReply> {
    if !is_json(&req) {
        return Err(Box::new(unexpected()));
    }
    let bytes = Bytes::from_request(req, state).await.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            error(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large.")
        } else {
            json_response(StatusCode::BAD_REQUEST, json!({"message": BAD_REQUEST_MSG}))
        }
    })?;
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|_| json_response(StatusCode::BAD_REQUEST, json!({"message": BAD_REQUEST_MSG})))?;
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(Box::new(json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"message": "Internal Server Error"}),
        ))),
    }
}

/// Body extractor for the `/api/v1` handlers deserialized with serde.
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let map = read_json_object(req, state).await.map_err(|r| *r)?;
        let value = Value::Object(map);
        match serde_path_to_error::deserialize::<_, T>(value) {
            Ok(t) => Ok(ApiJson(t)),
            Err(e) => Err(serde_error_to_fields(&e).into_response()),
        }
    }
}

/// Translate a serde error into marshmallow-style field errors.
pub fn serde_error_to_fields(e: &serde_path_to_error::Error<serde_json::Error>) -> FieldErrors {
    let mut f = FieldErrors::default();
    let msg = e.inner().to_string();
    let path = e.path().to_string();
    if let Some(field) = between(&msg, "missing field `", "`") {
        let full = if path == "." || path.is_empty() {
            field.to_string()
        } else {
            format!("{}.{}", path, field)
        };
        f.add(&full, "Missing data for required field.");
    } else if let Some(field) = between(&msg, "unknown field `", "`") {
        f.add(field, "Unknown field.");
    } else {
        let field = if path == "." || path.is_empty() {
            "_schema"
        } else {
            path.as_str()
        };
        let text = if msg.contains("expected a string") {
            "Not a valid string."
        } else if msg.contains("expected a boolean") {
            "Not a valid boolean."
        } else if msg.contains("expected i") || msg.contains("expected u") {
            "Not a valid integer."
        } else if msg.contains("expected f") || msg.contains("number") {
            "Not a valid number."
        } else if msg.contains("unknown variant") {
            "Invalid value."
        } else {
            "Invalid input."
        };
        f.add(field, text);
    }
    f
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

/// Validate a body whose only field is `apikey` (ping, funds, analyzer...),
/// exactly as marshmallow does: required, string, length 1..256, no unknown
/// fields.
pub fn apikey_only(body: &Map<String, Value>) -> Result<String, FieldErrors> {
    let mut f = FieldErrors::default();
    for k in body.keys() {
        if k != "apikey" {
            f.add(k, "Unknown field.");
        }
    }
    let key = match body.get("apikey") {
        None => {
            f.add("apikey", "Missing data for required field.");
            None
        }
        Some(Value::String(s)) => {
            let n = s.chars().count();
            if n == 0 || n > 256 {
                f.add("apikey", "Length must be between 1 and 256.");
                None
            } else {
                Some(s.clone())
            }
        }
        Some(Value::Null) => {
            f.add("apikey", "Field may not be null.");
            None
        }
        Some(_) => {
            f.add("apikey", "Not a valid string.");
            None
        }
    };
    match key {
        Some(k) if f.is_empty() => Ok(k),
        _ => Err(f),
    }
}

/// What a valid webhook address gets while OpenAlgo is locked (KEY-01):
/// 503 with the locked sentence under `field`, the key that webhook's own
/// error answers use (`error` for Chartink, `message` for strategies).
/// Nothing is placed, and nothing counts against the caller. The address is
/// checked first, so only its holder learns OpenAlgo is locked.
pub fn webhook_while_locked(field: &str) -> Response {
    let mut body = Map::new();
    body.insert("status".into(), "error".into());
    body.insert(field.into(), crate::services::core::WEBHOOK_LOCKED.into());
    json_response(StatusCode::SERVICE_UNAVAILABLE, Value::Object(body))
}

/// `401 {"status":"error","message":"Not authenticated"}`
pub fn not_authenticated() -> Response {
    error(StatusCode::UNAUTHORIZED, "Not authenticated")
}

impl IntoResponse for crate::error::AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            crate::error::AppError::Validation(_) => StatusCode::BAD_REQUEST,
            crate::error::AppError::Auth(_) | crate::error::AppError::Locked => {
                StatusCode::UNAUTHORIZED
            }
            crate::error::AppError::NotFound(_) => StatusCode::NOT_FOUND,
            crate::error::AppError::KeychainUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            crate::error::AppError::KeysDoNotMatch | crate::error::AppError::VaultUnreadable => {
                StatusCode::CONFLICT
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!("Request failed: {}", self);
        }
        // The ways out the sign-in page offers when an install's keys cannot
        // be opened (KEY-01).
        let recovery = match &self {
            crate::error::AppError::KeychainUnavailable => {
                Some(("keychain_unreadable", vec!["retry_keychain", "start_fresh"]))
            }
            crate::error::AppError::KeysDoNotMatch => Some(("keys_missing", vec!["start_fresh"])),
            crate::error::AppError::VaultUnreadable => {
                Some(("vault_unreadable", vec!["start_fresh"]))
            }
            _ => None,
        };
        match recovery {
            Some((code, ways)) => json_response(
                status,
                json!({
                    "status": "error",
                    "message": self.client_message(),
                    "code": code,
                    "recovery": ways,
                }),
            ),
            None => error(status, self.client_message()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// The web's `redact_url_credentials` cases for a path (R-05).
    #[test]
    fn a_webhook_credential_is_masked_in_a_path() {
        for (path, shown) in [
            ("/flow/webhook/abc", "/flow/webhook/<redacted>"),
            ("/flow/webhook/abc/NIFTY", "/flow/webhook/<redacted>/NIFTY"),
            ("/strategy/webhook/abc", "/strategy/webhook/<redacted>"),
            (
                "/Chartink/Webhook/abc?x=1",
                "/Chartink/Webhook/<redacted>?x=1",
            ),
            ("/flow/webhook/", "/flow/webhook/"),
            (
                "/flow/webhook/a/flow/webhook/b",
                "/flow/webhook/<redacted>/flow/webhook/<redacted>",
            ),
            ("/api/v1/flow/webhook", "/api/v1/flow/webhook"),
            ("/flow/api/workflows/abc", "/flow/api/workflows/abc"),
        ] {
            assert_eq!(redact_path(path), shown, "{}", path);
        }
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Q {
        apikey: String,
        symbol: String,
        quantity: i64,
    }

    fn err_of(v: Value) -> FieldErrors {
        let e = serde_path_to_error::deserialize::<_, Q>(v).unwrap_err();
        serde_error_to_fields(&e)
    }

    /// KEY-01: the sign-in page reads the ways out of a key error from
    /// the answer; ordinary errors keep the web's two fields.
    #[tokio::test]
    async fn key_errors_carry_their_ways_out() {
        use http_body_util::BodyExt;
        async fn body(e: crate::error::AppError) -> (StatusCode, Value) {
            let r = e.into_response();
            let s = r.status();
            let b = r.into_body().collect().await.unwrap().to_bytes();
            (s, serde_json::from_slice(&b).unwrap())
        }
        let (s, v) = body(crate::error::AppError::KeychainUnavailable).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(v["code"], "keychain_unreadable");
        assert_eq!(v["recovery"], json!(["retry_keychain", "start_fresh"]));
        assert_eq!(v["message"], crate::error::KEYCHAIN_UNAVAILABLE_MESSAGE);
        let (s, v) = body(crate::error::AppError::KeysDoNotMatch).await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert_eq!(v["recovery"], json!(["start_fresh"]));
        let (_, v) = body(crate::error::AppError::VaultUnreadable).await;
        assert_eq!(v["code"], "vault_unreadable");
        let (s, v) = body(crate::error::AppError::Validation(
            "Enter a username.".into(),
        ))
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(
            v,
            json!({"status": "error", "message": "Enter a username."})
        );
    }

    #[test]
    fn maps_serde_errors_to_marshmallow_messages() {
        let f = err_of(json!({"apikey": "k", "quantity": 1}));
        assert_eq!(f.0["symbol"], vec!["Missing data for required field."]);
        let f = err_of(json!({"apikey": "k", "symbol": "S", "quantity": 1, "foo": 1}));
        assert_eq!(f.0["foo"], vec!["Unknown field."]);
        let f = err_of(json!({"apikey": "k", "symbol": 5, "quantity": 1}));
        assert_eq!(f.0["symbol"], vec!["Not a valid string."]);
        let f = err_of(json!({"apikey": "k", "symbol": "S", "quantity": "x"}));
        assert_eq!(f.0["quantity"], vec!["Not a valid integer."]);
    }

    #[test]
    fn apikey_only_rules() {
        let m = |v: Value| v.as_object().unwrap().clone();
        assert!(apikey_only(&m(json!({"apikey": "k"}))).is_ok());
        assert!(apikey_only(&m(json!({}))).is_err());
        assert!(apikey_only(&m(json!({"apikey": ""}))).is_err());
        assert!(apikey_only(&m(json!({"apikey": "k", "foo": "bar"}))).is_err());
        assert!(apikey_only(&m(json!({"apikey": "x".repeat(257)}))).is_err());
    }
}
