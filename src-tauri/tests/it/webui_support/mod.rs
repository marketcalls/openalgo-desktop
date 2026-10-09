//! Shared harness for the web-UI route tests: a full app context in a temp
//! directory (memory keystore, manual clock, no broker adapters) driven
//! in-process with `tower::ServiceExt::oneshot`.
#![allow(dead_code)]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use openalgo_desktop_lib::brokers::BrokerRegistry;
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::services::auth_service::AuthService;
use openalgo_desktop_lib::state::{AppState, OpenOptions};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

pub const USER: &str = "trader";

pub fn ist(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(y, m, d, h, mi, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

pub struct H {
    pub ctx: Arc<AppState>,
    pub clock: Arc<ManualClock>,
    pub dir: tempfile::TempDir,
}

impl H {
    pub fn new() -> Self {
        Self::at(ist(2026, 10, 5, 10, 0))
    }

    pub fn at(now: DateTime<Utc>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::new(now);
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: clock.clone(),
                brokers: Arc::new(BrokerRegistry::with(vec![])),
            },
        )
        .unwrap();
        H { ctx, clock, dir }
    }

    /// Account and API key; returns the key.
    pub fn setup(&self) -> String {
        AuthService::setup(&self.ctx, USER, "trader@example.com", "Secret@123").unwrap();
        openalgo_desktop_lib::services::apikey_service::ApiKeyService::current(&self.ctx)
            .unwrap()
            .unwrap()
            .expose()
            .to_string()
    }

    /// (cookie header, csrf token). Signed in when `user`.
    pub fn session(&self, user: bool) -> (String, String) {
        let s = self.ctx.sessions.create(self.ctx.now());
        if user {
            self.ctx
                .sessions
                .update(&s.id, |x| x.user = Some(USER.into()));
        }
        (format!("session={}", s.id), s.csrf_token)
    }

    pub async fn send_from(
        &self,
        mut req: Request<Body>,
        ip: [u8; 4],
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((ip, 40000))));
        crate::with_host(&mut req, &self.ctx);
        let app = openalgo_desktop_lib::server::app(self.ctx.clone());
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, headers, body)
    }

    pub async fn send(&self, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        self.send_from(req, [127, 0, 0, 1]).await
    }

    pub async fn json(&self, req: Request<Body>) -> (StatusCode, Value) {
        let (s, _, b) = self.send(req).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }
}

pub fn req(method: Method, path: &str, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::ACCEPT, "application/json");
    let body = match body {
        Some(v) => {
            b = b.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    b.body(body).unwrap()
}

pub fn get(path: &str) -> Request<Body> {
    req(Method::GET, path, None)
}

pub fn with(mut r: Request<Body>, cookie: &str, csrf: Option<&str>) -> Request<Body> {
    r.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    if let Some(t) = csrf {
        r.headers_mut().insert("x-csrftoken", t.parse().unwrap());
    }
    r
}

/// Signed-in harness plus its cookie and CSRF token.
pub fn signed_in() -> (H, String, String) {
    let h = H::new();
    h.setup();
    let (c, t) = h.session(true);
    (h, c, t)
}

pub fn fixture(rel: &str) -> Value {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/web/rest")
        .join(rel);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}
