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

pub fn json_response(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// `{"status":"error","message":msg}`
pub fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    json_response(status, json!({"status": "error", "message": msg.into()}))
}

pub fn not_found(path: &str) -> Response {
    json_response(
        StatusCode::NOT_FOUND,
        json!({"status": "error", "message": "Not found", "path": path}),
    )
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
) -> Result<Map<String, Value>, Response> {
    if !is_json(&req) {
        return Err(unexpected());
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
        _ => Err(json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"message": "Internal Server Error"}),
        )),
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
        let map = read_json_object(req, state).await?;
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
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!("Request failed: {}", self);
        }
        error(status, self.client_message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

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
