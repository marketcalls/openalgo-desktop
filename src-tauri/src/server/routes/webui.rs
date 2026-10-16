//! Helpers shared by the web-UI page routes (admin, monitoring, settings).

use crate::server::envelope::{error, json_response};
use axum::{
    body::Bytes,
    extract::{FromRequest, Request},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value};

/// A field that should hold a whole number did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotANumber;

/// JSON object body. A missing, empty or malformed body is an empty object
/// (the web reads these with `request.get_json()` and defaults every field).
pub struct JsonBody(pub Map<String, Value>);

impl<S: Send + Sync> FromRequest<S> for JsonBody {
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|_| error(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large."))?;
        let map = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        Ok(JsonBody(map))
    }
}

impl JsonBody {
    pub fn str(&self, k: &str) -> Option<String> {
        match self.0.get(k) {
            Some(Value::String(s)) => Some(s.trim().to_string()),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(Value::Bool(b)) => Some(b.to_string()),
            _ => None,
        }
    }

    pub fn non_empty(&self, k: &str) -> Option<String> {
        self.str(k).filter(|s| !s.is_empty())
    }

    /// An integer the way Python's `int()` reads JSON numbers and strings.
    pub fn int(&self, k: &str) -> Result<Option<i64>, NotANumber> {
        match self.0.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => n
                .as_i64()
                .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                .map(Some)
                .ok_or(NotANumber),
            Some(Value::String(s)) => s.trim().parse::<i64>().map(Some).map_err(|_| NotANumber),
            Some(Value::Bool(b)) => Ok(Some(*b as i64)),
            _ => Err(NotANumber),
        }
    }

    pub fn bool(&self, k: &str) -> bool {
        match self.0.get(k) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
            Some(Value::String(s)) => matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            ),
            _ => false,
        }
    }
}

/// A numeric path id as the web's `<int:id>` converter reads it: digits
/// only. `None` answers the web's 404.
pub fn int_id(raw: &str) -> Option<i64> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

pub fn ok(v: Value) -> Response {
    json_response(StatusCode::OK, v)
}

/// The web's no-store header on diagnostics responses.
pub fn no_store(mut r: Response) -> Response {
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    r
}

/// A file download with a fixed, server-chosen file name.
pub fn download(body: String, mime: &'static str, filename: &str) -> Response {
    attachment((StatusCode::OK, body).into_response(), mime, filename)
}

/// [`download`] for a body sent as it is produced: `body` yields the text
/// in pieces, and an error ends the download where it happened.
pub fn download_stream<S>(body: S, mime: &'static str, filename: &str) -> Response
where
    S: futures_util::Stream<Item = crate::error::Result<String>> + Send + 'static,
{
    let body = axum::body::Body::from_stream(body);
    attachment((StatusCode::OK, body).into_response(), mime, filename)
}

/// `r` as a file download named `filename`, never cached.
fn attachment(mut r: Response, mime: &'static str, filename: &str) -> Response {
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename={}", filename)) {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    no_store(r)
}

/// One CSV field (RFC 4180 quoting).
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn csv_row(fields: &[String]) -> String {
    let mut line = fields
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",");
    line.push_str("\r\n");
    line
}

/// Internal failure: logged once here, trader-facing text out.
pub fn failed(what: &str, e: impl std::fmt::Display, msg: &str) -> Response {
    tracing::error!("{} failed: {}", what, e);
    error(StatusCode::INTERNAL_SERVER_ERROR, msg)
}
