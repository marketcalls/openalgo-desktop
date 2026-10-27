//! The voice routes end to end (the web's `test/test_agent_voice.py`):
//! through the HTTP app, against a fake realtime session server on an
//! ephemeral port. Nothing here reaches OpenAI.

mod mint;
mod runs;
mod settings;
mod window;

use crate::agent::chat::tests::{harness, Harness};
use crate::agent::testing::Reply;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::Arc;

/// A voice key for tests: long enough to be accepted, plainly not a real one.
pub(crate) const KEY: &str = "testkey-testkey-testkey";

/// A short browser offer, CRLF-terminated.
pub(crate) const OFFER: &str =
    "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 96\r\n";

/// What the fake provider answers when nothing is scripted.
pub(crate) const ANSWER: &str = "v=0\r\nanswer\r\n";

/// One mint request as the fake provider received it.
#[derive(Debug, Clone)]
pub(crate) struct Minted {
    pub path: String,
    pub authorization: String,
    pub body: Value,
}

#[derive(Default)]
struct Script {
    replies: Mutex<VecDeque<(u16, String)>>,
    calls: Mutex<Vec<Minted>>,
}

/// A stand-in for the provider's sessions endpoint: records each request
/// and answers the next scripted `(status, body)`, else a good answer.
pub(crate) struct FakeRealtime {
    pub url: String,
    script: Arc<Script>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeRealtime {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answer_mint(
    State(s): State<Arc<Script>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    s.calls.lock().push(Minted {
        path: uri.path().to_string(),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string(),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    });
    let (status, text) = s
        .replies
        .lock()
        .pop_front()
        .unwrap_or((200, json!({"transport": {"sdp": ANSWER}}).to_string()));
    (StatusCode::from_u16(status).unwrap(), text).into_response()
}

impl FakeRealtime {
    pub async fn start() -> FakeRealtime {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let script = Arc::new(Script::default());
        let app = axum::Router::new()
            .fallback(answer_mint)
            .with_state(script.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeRealtime {
            url: format!("http://127.0.0.1:{}/v1/live/sessions", port),
            script,
            task,
        }
    }

    pub fn reply(&self, status: u16, body: &str) {
        self.script
            .replies
            .lock()
            .push_back((status, body.to_string()));
    }

    pub fn calls(&self) -> Vec<Minted> {
        self.script.calls.lock().clone()
    }
}

/// A signed-in trader with the fake model provider answering `script`, the
/// mint pointed at a fresh fake realtime server.
pub(crate) async fn voice_harness(script: Vec<Reply>) -> (Harness, FakeRealtime) {
    let h = harness(script).await;
    let fake = FakeRealtime::start().await;
    h.ctx().agent.voice().set_sessions_url(&fake.url);
    (h, fake)
}

/// Voice switched on with the test key stored.
pub(crate) async fn switch_on(h: &Harness) {
    let (s, v) = h
        .json("PUT", "/agent/api/voice/key", Some(json!({"api_key": KEY})))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    put_voice(h, json!({"voice_enabled": true})).await;
}

/// `PUT /agent/api/voice`, which must succeed.
pub(crate) async fn put_voice(h: &Harness, body: Value) -> Value {
    let (s, v) = h.json("PUT", "/agent/api/voice", Some(body)).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    v
}

/// POST an offer to the mint route as the page does (`application/sdp`).
pub(crate) async fn mint(h: &Harness, offer: &str) -> (StatusCode, String, String) {
    let (mut parts, _) = h
        .request("POST", "/agent/api/voice/session", None)
        .into_parts();
    parts
        .headers
        .insert(header::CONTENT_TYPE, "application/sdp".parse().unwrap());
    let req = Request::from_parts(parts, Body::from(offer.to_string()));
    let resp = tower::ServiceExt::oneshot(crate::server::app(h.ctx().clone()), req)
        .await
        .unwrap();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = resp.status();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    (
        status,
        content_type,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The message of a JSON error body.
pub(crate) fn message_of(body: &str) -> String {
    serde_json::from_str::<Value>(body).unwrap_or(Value::Null)["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}
