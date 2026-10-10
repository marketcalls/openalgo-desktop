//! In-process HTTP tests: the full router (every layer) driven with
//! `tower::ServiceExt::oneshot`, against the golden fixtures recorded from
//! OpenAlgo web and against the session, CSRF, guard and OAuth rules.

use crate::brokers::mock::MockBroker;
use crate::brokers::BrokerRegistry;
use crate::server::routes::{self, Access};
use crate::services::auth_service::AuthService;
use crate::services::broker_auth_service::BrokerAuthService;
use crate::state::testing::{build, TestCtx};
use crate::state::{AppState, BrokerSession};
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

const USER: &str = "trader";
const EMAIL: &str = "trader@example.com";
const PASSWORD: &str = "Secret@123";

fn ist(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(y, m, d, h, mi, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

struct H {
    t: TestCtx,
    mock: Arc<MockBroker>,
}

impl H {
    fn new() -> Self {
        let mock = Arc::new(MockBroker::new("zerodha"));
        let t = build(
            BrokerRegistry::with(vec![mock.clone() as Arc<dyn crate::brokers::Broker>]),
            ist(2026, 10, 5, 10, 0),
        );
        // Pin the limiter clock: rate-limit tests must not depend on how fast
        // the runner sends requests.
        t.ctx.limiter.freeze(Some(std::time::Instant::now()));
        H { t, mock }
    }

    fn ctx(&self) -> &Arc<AppState> {
        &self.t.ctx
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let app = crate::server::app(self.ctx().clone());
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

    async fn json(&self, req: Request<Body>) -> (StatusCode, Value) {
        let (s, _, b) = self.send(req).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    /// Account + API key; returns the key.
    fn setup(&self) -> String {
        AuthService::setup(self.ctx(), USER, EMAIL, PASSWORD).unwrap();
        crate::services::apikey_service::ApiKeyService::current(self.ctx())
            .unwrap()
            .unwrap()
            .expose()
            .to_string()
    }

    fn connect_broker(&self) {
        let now = self.ctx().now();
        BrokerAuthService::persist(
            self.ctx(),
            &BrokerSession {
                broker_id: "zerodha".into(),
                auth_token: "mock-access-token".into(),
                feed_token: None,
                user_id: "AB1234".into(),
                user_name: None,
                authenticated_at: now,
            },
        )
        .unwrap();
    }

    fn save_broker_credentials(&self) {
        let conn = self.ctx().sqlite.conn().unwrap();
        crate::db::sqlite::credentials::save(
            &conn,
            &self.ctx().security,
            "zerodha",
            crate::db::sqlite::credentials::CredentialUpdate {
                api_key: Some("kiteapikey".into()),
                api_secret: Some("kitesecret".into()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::config::save(
            &conn,
            &crate::config::ServerConfigUpdate {
                active_broker: Some("zerodha".into()),
                ..Default::default()
            },
        )
        .unwrap();
        drop(conn);
        self.ctx().reload_config().unwrap();
    }

    /// A browser session (cookie, csrf). Signed in when `user` is true.
    fn session(&self, user: bool) -> (String, String) {
        let s = self.ctx().sessions.create(self.ctx().now());
        if user {
            self.ctx()
                .sessions
                .update(&s.id, |x| x.user = Some(USER.into()));
        }
        (format!("session={}", s.id), s.csrf_token)
    }
}

fn post_json(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

/// A page navigation made inside OpenAlgo (the browser's `Sec-Fetch-Site`).
fn in_app(mut r: Request<Body>) -> Request<Body> {
    r.headers_mut()
        .insert("sec-fetch-site", "same-origin".parse().unwrap());
    r
}

fn with_session(mut r: Request<Body>, cookie: &str, csrf: Option<&str>) -> Request<Body> {
    r.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    if let Some(t) = csrf {
        r.headers_mut().insert("x-csrftoken", t.parse().unwrap());
    }
    r
}

fn form(path: &str, fields: &[(&str, &str)]) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(serde_urlencoded::to_string(fields).unwrap()))
        .unwrap()
}

fn multipart(path: &str, fields: &[(&str, &str)]) -> Request<Body> {
    let boundary = "XBOUNDARYX";
    let mut body = String::new();
    for (k, v) in fields {
        body.push_str(&format!(
            "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
            boundary, k, v
        ));
    }
    body.push_str(&format!("--{}--\r\n", boundary));
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={}", boundary),
        )
        .body(Body::from(body))
        .unwrap()
}

fn cookie_from(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("session=") && !c.contains("Max-Age=0"))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
}

// Findings of the 2026-10-09 security review, one test group per finding.
mod security;
// Every catalogue broker's sign-in, started the way the broker page does.
mod sign_in;
// Codex review evaluation B: sign-out, revocation, keys, broker settings.
mod credentials_lifecycle;

// ------------------------------------------------------------------ fixtures

fn fixture(rel: &str) -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rest")
        .join(rel);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

/// Build the recorded request, substituting the real API key.
fn fixture_request(f: &Value, api_key: &str) -> Request<Body> {
    let req = &f["request"];
    let method = Method::from_bytes(req["method"].as_str().unwrap().as_bytes()).unwrap();
    let path = req["path"].as_str().unwrap().replace("<APIKEY>", api_key);
    let mut b = Request::builder().method(method).uri(path);
    if let Some(h) = req["headers"].as_object() {
        for (k, v) in h {
            b = b.header(k.as_str(), v.as_str().unwrap().replace("<APIKEY>", api_key));
        }
    }
    let body = match &req["body"] {
        Value::Null => Body::empty(),
        Value::String(s) => Body::from(s.replace("<APIKEY>", api_key)),
        other => Body::from(other.to_string().replace("<APIKEY>", api_key)),
    };
    b.body(body).unwrap()
}

/// Same JSON shape: same types, same object keys, recursively. Strings and
/// numbers may differ (market values, timestamps).
fn same_shape(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).map(|w| same_shape(v, w)).unwrap_or(false))
        }
        (Value::Array(x), Value::Array(y)) => match (x.first(), y.first()) {
            (Some(p), Some(q)) => same_shape(p, q),
            _ => true,
        },
        (Value::Number(_), Value::Number(_)) => true,
        (Value::String(_), Value::String(_)) => true,
        (Value::Bool(_), Value::Bool(_)) => true,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

/// Error bodies must match exactly; success bodies by shape.
async fn check_fixture(h: &H, rel: &str, key: &str) {
    let f = fixture(rel);
    let (status, headers, body) = h.send(fixture_request(&f, key)).await;
    let want_status = f["response"]["status_code"].as_u64().unwrap() as u16;
    assert_eq!(status.as_u16(), want_status, "{}: status", rel);
    let want = &f["response"]["body"];
    if let Some(raw) = want.get("_raw_text") {
        let ct = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/html"), "{}: content type {}", rel, ct);
        assert!(raw.as_str().unwrap().starts_with("<!doctype html>"));
        return;
    }
    let got: Value = serde_json::from_slice(&body).unwrap_or_else(|_| {
        panic!(
            "{}: body is not JSON: {:?}",
            rel,
            String::from_utf8_lossy(&body)
        )
    });
    if want_status >= 400 {
        assert_eq!(&got, want, "{}: body", rel);
    } else {
        assert!(
            same_shape(want, &got),
            "{}: shape\nwant {}\ngot  {}",
            rel,
            want,
            got
        );
    }
    assert!(headers.get("x-ratelimit-limit").is_none());
}

#[tokio::test]
async fn error_fixtures_match_the_web() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    for rel in [
        "errors/empty_apikey_string.json",
        "errors/empty_body_json_content_type.json",
        "errors/invalid_apikey_funds.json",
        "errors/invalid_apikey_quotes.json",
        "errors/json_array_body.json",
        "errors/malformed_json_quotes.json",
        "errors/missing_apikey_funds.json",
        "errors/missing_apikey_quotes.json",
        "errors/missing_required_field_quotes_symbol.json",
        "errors/non_json_content_type.json",
        "errors/trailing_slash_ping.json",
        "errors/unknown_route.json",
        "errors/unknown_route_outside_api.json",
        "errors/unknown_route_post.json",
        "errors/wrong_method_get_on_funds.json",
        "errors/wrong_method_get_on_quotes.json",
        "errors/wrong_method_put_on_ping.json",
        "errors/rate_limit_probe_no_429.json",
    ] {
        check_fixture(&h, rel, &key).await;
    }
}

#[tokio::test]
async fn ping_fixtures_match_the_web() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    for rel in [
        "ping/apikey_in_body.json",
        "ping/apikey_in_header_and_body.json",
        "ping/apikey_in_x_api_key_header.json",
        "ping/extra_unknown_field.json",
    ] {
        check_fixture(&h, rel, &key).await;
    }
    // The broker name is reported exactly.
    let (_, v) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(
        v,
        json!({"status": "success", "data": {"message": "pong", "broker": "zerodha"}})
    );
}

#[tokio::test]
async fn analyzer_fixtures_match_the_web() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    h.ctx().sqlite.set_analyze_mode(true).unwrap();
    check_fixture(&h, "analyzer/status.json", &key).await;
    let (_, v) = h
        .json(post_json("/api/v1/analyzer", json!({"apikey": key})))
        .await;
    assert_eq!(v["data"]["mode"], "analyze");
    assert_eq!(v["data"]["analyze_mode"], true);
    h.ctx().sqlite.set_analyze_mode(false).unwrap();
    check_fixture(&h, "analyzer/status_while_live.json", &key).await;
}

#[tokio::test]
async fn funds_fixtures_match_the_web_in_analyze_mode() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    h.ctx().sqlite.set_analyze_mode(true).unwrap();
    for rel in [
        "funds/apikey_in_body.json",
        "funds/apikey_in_header_and_body.json",
        "funds/apikey_in_x_api_key_header.json",
        "funds/after_activity.json",
        "funds/final.json",
    ] {
        check_fixture(&h, rel, &key).await;
    }
}

#[tokio::test]
async fn live_funds_go_through_the_broker_with_web_string_values() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let (s, v) = h
        .json(post_json("/api/v1/funds", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"status": "success", "data": {
            "availablecash": "125000.50", "collateral": "1000.00",
            "m2mrealized": "0.00", "m2munrealized": "0.00", "utiliseddebits": "2500.25",
        }})
    );
    assert!(*h.mock.funds_calls.lock() >= 1);
    // Broker failure: JSON envelope, broker's message, no internals.
    *h.mock.funds_ok.lock() = false;
    let (s, v) = h
        .json(post_json("/api/v1/funds", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(v["status"], "error");
}

#[tokio::test]
async fn valid_key_without_broker_session_is_invalid_like_the_web() {
    let h = H::new();
    let key = h.setup();
    let (s, v) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(
        v,
        json!({"status": "error", "message": "Invalid openalgo apikey"})
    );
}

#[tokio::test]
async fn api_rate_limit_is_100_per_second_per_ip_with_flask_body() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    for _ in 0..100 {
        let (s, _) = h
            .json(post_json("/api/v1/ping", json!({"apikey": key})))
            .await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, v) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v, json!({"message": "100 per 1 second"}));
}

#[tokio::test]
async fn order_bucket_429_matches_fixture() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let f = fixture("errors/rate_limit_429_placeorder.json");
    for _ in 0..10 {
        let _ = h.send(fixture_request(&f, &key)).await;
    }
    check_fixture(&h, "errors/rate_limit_429_placeorder.json", &key).await;
}

#[tokio::test]
async fn bad_api_keys_are_throttled_per_ip() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    // Another device (loopback is never locked out: security review S-02).
    let lan: std::net::IpAddr = "192.168.1.50".parse().unwrap();
    for _ in 0..12 {
        let mut r = post_json("/api/v1/ping", json!({"apikey": "wrong"}));
        r.extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(
                lan, 50000,
            )));
        r.headers_mut().insert(
            header::HOST,
            format!("127.0.0.1:{}", h.ctx().server_config().http_port)
                .parse()
                .unwrap(),
        );
        let (s, v) = h.json(r).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["message"], "Invalid openalgo apikey");
    }
    assert!(h.ctx().limiter.is_exhausted(
        crate::server::ratelimit::Bucket::ApiKeyFail,
        lan,
        h.ctx().limiter.now()
    ));
}

// ------------------------------------------------------------ guard coverage

fn concrete(path: &str) -> String {
    path.replace("{broker}", "zerodha")
        .replace("{id}", "1")
        .replace("{alert_id}", "1")
        .replace("{exchange}", "NSE")
        .replace("{*rest}", "status")
}

#[tokio::test]
async fn every_user_route_rejects_without_a_signed_in_session() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    let (anon_cookie, anon_csrf) = h.session(false);
    let mut checked = 0;
    for spec in routes::table() {
        if !matches!(spec.access, Access::User | Access::UserJson) {
            continue;
        }
        let path = concrete(spec.path);
        let build = || {
            Request::builder()
                .method(spec.method.clone())
                .uri(&path)
                .header(header::ACCEPT, "application/json")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap()
        };
        // No cookie at all.
        let (s, _, _) = h.send(build()).await;
        assert!(
            s == StatusCode::UNAUTHORIZED || s == StatusCode::BAD_REQUEST,
            "{} {} without session gave {}",
            spec.method,
            path,
            s
        );
        // A real but anonymous session with a valid CSRF token: the guard
        // itself must answer 401.
        let (s, _, b) = h
            .send(with_session(build(), &anon_cookie, Some(&anon_csrf)))
            .await;
        assert_eq!(
            s,
            StatusCode::UNAUTHORIZED,
            "{} {} anonymous",
            spec.method,
            path
        );
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(
            v,
            json!({"status": "error", "message": "Not authenticated"})
        );
        checked += 1;
    }
    assert!(checked >= 18, "only {} user routes", checked);
}

/// The Agent is deferred (CLAUDE.md): every `/agent/api` call from its pages
/// gets the web's error envelope with a trader-facing sentence and 503 (the
/// web's status when it cannot run the agent), never the page's 404, while
/// the `/agent` pages themselves are still served.
#[tokio::test]
async fn agent_api_says_the_agent_is_not_available_yet() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    let want = json!({"status": "error", "message": routes::agent::UNAVAILABLE_MESSAGE});
    assert!(routes::agent::UNAVAILABLE_MESSAGE.contains("not available in OpenAlgo Desktop yet"));
    for (method, path) in [
        (Method::GET, "/agent/api/status"),
        (Method::GET, "/agent/api"),
        (Method::GET, "/agent/api/catalog/models?provider=openai"),
        (Method::POST, "/agent/api/chat/stream"),
        (Method::PUT, "/agent/api/settings"),
        (Method::PATCH, "/agent/api/models/1"),
        (Method::DELETE, "/agent/api/conversations/7"),
    ] {
        let req = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let (s, v) = h.json(with_session(req, &cookie, Some(&csrf))).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{} {}", method, path);
        assert_eq!(v, want, "{} {}", method, path);
    }
    let (s, _, body) = h.send(with_session(get("/agent"), &cookie, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("<!doctype html"));
}

#[tokio::test]
async fn public_route_list_is_exactly_the_reviewed_one() {
    let public: Vec<String> = routes::table()
        .into_iter()
        .filter(|s| matches!(s.access, Access::Public | Access::BrokerCallback))
        .map(|s| format!("{} {}", s.method, s.path))
        .collect();
    assert_eq!(
        public,
        vec![
            "GET /auth/csrf-token",
            "GET /auth/check-setup",
            "GET /auth/app-info",
            "GET /auth/session-status",
            "GET /auth/broker-config",
            "POST /setup",
            "GET /auth/login",
            "POST /auth/login",
            "POST /auth/login/totp",
            "GET /auth/logout",
            "POST /auth/logout",
            "POST /auth/reset-password",
            // Secret-verified webhook (the URL token is the credential).
            "POST /strategy/webhook/{token}",
            // MCP: bearer token (crate::mcp::http).
            "POST /mcp",
            "GET /mcp",
            "GET /mcp/healthz",
            "GET /{broker}/callback",
            "POST /{broker}/callback",
            // The OpenScript runner page's channel: each call carries its
            // run's secret (handed to that page only, in its URL fragment).
            "GET /openscript/runner/host/{run}/spec",
            "GET /openscript/runner/host/{run}/bars",
            "GET /openscript/runner/host/{run}/inbox",
            "POST /openscript/runner/host/{run}/intents",
            "POST /openscript/runner/host/{run}/log",
            "POST /openscript/runner/host/{run}/ended",
            // Secret-verified Chartink webhook (the URL id is the credential).
            "POST /chartink/webhook/{webhook_id}",
        ]
    );
}

#[tokio::test]
async fn apikey_page_navigation_gets_the_spa_but_json_needs_the_user() {
    let h = H::new();
    h.setup();
    let (s, headers, _) = h.send(get("/apikey")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let mut r = get("/apikey");
    r.headers_mut()
        .insert(header::ACCEPT, "application/json".parse().unwrap());
    let (s, _) = h.json(r).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------- CSRF

#[tokio::test]
async fn csrf_token_endpoint_creates_a_session_cookie() {
    let h = H::new();
    let (s, headers, b) = h.send(get("/auth/csrf-token")).await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert!(v["csrf_token"].as_str().unwrap().len() >= 40);
    let set = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(set.contains("HttpOnly"));
    assert!(set.contains("SameSite=Lax"));
    // Same session, same token.
    let cookie = cookie_from(&headers).unwrap();
    let (_, v2) = h
        .json(with_session(get("/auth/csrf-token"), &cookie, None))
        .await;
    assert_eq!(v2["csrf_token"], v["csrf_token"]);
}

#[tokio::test]
async fn csrf_is_enforced_by_header_or_form_field() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    let body = json!({"user_id": USER, "mode": "semi_auto"});
    // Missing token.
    let (s, _) = h
        .json(with_session(
            post_json("/apikey/mode", body.clone()),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Wrong token.
    let (s, _) = h
        .json(with_session(
            post_json("/apikey/mode", body.clone()),
            &cookie,
            Some("nope"),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Header.
    let (s, v) = h
        .json(with_session(
            post_json("/apikey/mode", body),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["mode"], "semi_auto");
    // Form field, urlencoded and multipart.
    let (s, _) = h
        .json(with_session(
            form(
                "/apikey/mode",
                &[("user_id", USER), ("mode", "auto"), ("csrf_token", &csrf)],
            ),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .json(with_session(
            multipart(
                "/apikey/mode",
                &[("user_id", USER), ("mode", "auto"), ("csrf_token", &csrf)],
            ),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .json(with_session(
            multipart(
                "/apikey/mode",
                &[("user_id", USER), ("mode", "auto"), ("csrf_token", "bad")],
            ),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn cross_site_writes_and_foreign_hosts_are_blocked() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    let mut r = with_session(post_json("/auth/logout", json!({})), &cookie, Some(&csrf));
    r.headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    let (s, _) = h.json(r).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let mut r = with_session(post_json("/auth/logout", json!({})), &cookie, Some(&csrf));
    r.headers_mut()
        .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
    let (s, _) = h.json(r).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // DNS rebinding: foreign Host header.
    let mut r = get("/auth/check-setup");
    r.headers_mut()
        .insert(header::HOST, "evil.example:5500".parse().unwrap());
    let (s, _) = h.json(r).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let port = h.ctx().server_config().http_port;
    let mut r = get("/auth/check-setup");
    r.headers_mut()
        .insert(header::HOST, format!("127.0.0.1:{}", port).parse().unwrap());
    let (s, _) = h.json(r).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn cors_allows_only_the_app_origin() {
    let h = H::new();
    let port = h.ctx().server_config().http_port;
    let mut r = get("/auth/check-setup");
    r.headers_mut().insert(
        header::ORIGIN,
        format!("http://127.0.0.1:{}", port).parse().unwrap(),
    );
    let (_, headers, _) = h.send(r).await;
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        format!("http://127.0.0.1:{}", port).as_str()
    );
    let mut r = get("/auth/check-setup");
    r.headers_mut()
        .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
    let (_, headers, _) = h.send(r).await;
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}

// ---------------------------------------------------- setup, login, sessions

#[tokio::test]
async fn setup_login_logout_lifecycle_like_the_web() {
    let h = H::new();
    let (s, v) = h.json(get("/auth/check-setup")).await;
    assert_eq!((s, v["needs_setup"].clone()), (StatusCode::OK, json!(true)));

    // Login before setup.
    let (s, v) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["redirect"], "/setup");

    // Weak password is refused with the web's message. Setup needs the
    // page's session and CSRF token, like the web's setup form.
    let (setup_cookie, setup_csrf) = h.session(false);
    let setup = |fields: &[(&str, &str)]| {
        with_session(
            multipart("/setup", fields),
            &setup_cookie,
            Some(&setup_csrf),
        )
    };
    let (s, v) = h
        .json(setup(&[
            ("username", USER),
            ("email", EMAIL),
            ("password", "weak"),
        ]))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Password must be at least 8 characters long");
    let (s, _) = h
        .json(setup(&[
            ("username", USER),
            ("email", EMAIL),
            ("password", PASSWORD),
        ]))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .json(setup(&[
            ("username", "x"),
            ("email", EMAIL),
            ("password", PASSWORD),
        ]))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "second setup refused");

    // Wrong password.
    let (s, v) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", "Wrong@123")],
        ))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        v,
        json!({"status": "error", "message": "Invalid credentials"})
    );

    // Right password: cookie, no broker yet.
    let (s, headers, b) = h
        .send(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&b).unwrap(),
        json!({"status": "success"})
    );
    let cookie = cookie_from(&headers).unwrap();
    let (_, v) = h
        .json(with_session(get("/auth/session-status"), &cookie, None))
        .await;
    assert_eq!(v["authenticated"], true);
    assert_eq!(v["logged_in"], false);
    assert_eq!(v["user"], USER);

    // Dashboard says broker not connected with the web's code.
    let (s, v) = h
        .json(with_session(get("/auth/dashboard-data"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(v["code"], "BROKER_SESSION_EXPIRED");

    // Connect, then the status carries the broker and the API key.
    h.connect_broker();
    let (_, v) = h
        .json(with_session(get("/auth/session-status"), &cookie, None))
        .await;
    assert_eq!(v["logged_in"], true);
    assert_eq!(v["broker"], "zerodha");
    assert_eq!(v["api_key"].as_str().unwrap().len(), 64);
    let (s, v) = h
        .json(with_session(get("/auth/dashboard-data"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["availablecash"], "125000.50");

    // Logout revokes the stored broker token and every session.
    let csrf = h
        .ctx()
        .sessions
        .get(cookie.trim_start_matches("session="), h.ctx().now())
        .unwrap()
        .csrf_token;
    let (s, headers, _) = h
        .send(with_session(
            post_json("/auth/logout", json!({})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert!(!h.ctx().is_broker_connected());
    assert_eq!(h.ctx().sessions.authenticated_count(), 0);
    let conn = h.ctx().sqlite.conn().unwrap();
    assert!(
        crate::db::sqlite::auth::latest_active(&conn, &h.ctx().security)
            .unwrap()
            .is_none()
    );
    let revoked: i64 = conn
        .query_row(
            "SELECT is_revoked FROM auth WHERE broker_id = 'zerodha'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revoked, 1);
    drop(conn);

    // Signing in again does not resume a revoked token.
    let (_, _, b) = h
        .send(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&b).unwrap(),
        json!({"status": "success"})
    );
}

#[tokio::test]
async fn login_resumes_a_fresh_broker_session_after_restart() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    // Simulate a restart: memory is empty, the encrypted row remains.
    h.ctx().set_broker_session(None);
    h.ctx().sessions.clear();
    let (s, v) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Broker session resumed", "redirect": "/dashboard", "broker": "zerodha"})
    );
    assert!(h.ctx().is_broker_connected());
}

#[tokio::test]
async fn resume_is_refused_when_the_broker_rejects_the_token() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    h.ctx().set_broker_session(None);
    *h.mock.funds_ok.lock() = false;
    let (_, v) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(v, json!({"status": "success"}));
    assert!(!h.ctx().is_broker_connected());
}

#[tokio::test]
async fn daily_boundary_ends_the_broker_session_at_three_ist() {
    let h = H::new(); // clock: 2026-10-05 10:00 IST
    h.setup();
    h.connect_broker();
    let (cookie, _) = h.session(true);
    let start = h.ctx().now();

    h.t.clock.set(ist(2026, 10, 6, 2, 59));
    assert!(h.ctx().is_broker_connected(), "still valid at 02:59");
    assert!(!crate::session::expire_if_crossed(h.ctx(), start).await);

    h.t.clock.set(ist(2026, 10, 6, 3, 0));
    assert!(!h.ctx().is_broker_connected(), "expired at 03:00");
    assert!(crate::session::expire_if_crossed(h.ctx(), ist(2026, 10, 6, 2, 59)).await);
    // Browser sessions dropped; stored row revoked.
    let (_, v) = h
        .json(with_session(get("/auth/session-status"), &cookie, None))
        .await;
    assert_eq!(v["authenticated"], false);
    let conn = h.ctx().sqlite.conn().unwrap();
    assert!(
        crate::db::sqlite::auth::latest_active(&conn, &h.ctx().security)
            .unwrap()
            .is_none()
    );
    drop(conn);
    // Firing again in the same day does nothing.
    assert!(!crate::session::expire_if_crossed(h.ctx(), ist(2026, 10, 6, 3, 0)).await);

    // A login after the boundary does not resume.
    let (_, v) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(v, json!({"status": "success"}));
}

#[tokio::test]
async fn stale_token_is_not_resumed_even_without_the_scheduler() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    h.ctx().set_broker_session(None);
    h.t.clock.set(ist(2026, 10, 6, 9, 0)); // next morning, scheduler never ran
    let (_, v) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(v, json!({"status": "success"}));
}

#[tokio::test]
async fn login_is_throttled_per_ip() {
    let h = H::new();
    h.setup();
    // The app's page opened from a device on the network (this computer has
    // the same limit: `security::s02_local_guessing_...`).
    let lan = |r: Request<Body>| {
        let mut r = r;
        r.extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(
                "192.168.1.50".parse().unwrap(),
                50000,
            )));
        let own = format!("127.0.0.1:{}", h.ctx().server_config().http_port);
        r.headers_mut().insert(header::HOST, own.parse().unwrap());
        r.headers_mut()
            .insert(header::ORIGIN, format!("http://{}", own).parse().unwrap());
        r
    };
    for _ in 0..5 {
        let (s, _) = h
            .json(lan(multipart(
                "/auth/login",
                &[("username", USER), ("password", "Wrong@123")],
            )))
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, v) = h
        .json(lan(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        )))
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        v["message"],
        "Too many login attempts. Please wait a minute and try again."
    );
}

#[tokio::test]
async fn change_password_signs_everyone_out() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    let (s, v) = h
        .json(with_session(
            multipart(
                "/auth/change-password",
                &[
                    ("old_password", PASSWORD),
                    ("new_password", "Newer@456"),
                    ("confirm_password", "Newer@456"),
                ],
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(h.ctx().sessions.len(), 0);
    let (s, _) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", "Newer@456")],
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn password_reset_with_totp_like_the_web() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(false);
    let secret = AuthService::profile(h.ctx(), USER)
        .unwrap()
        .unwrap()
        .1
        .unwrap();
    let code =
        crate::security::totp::generate(secret.expose(), h.ctx().now().timestamp() as u64).unwrap();
    let post = |body: Value| {
        with_session(
            post_json("/auth/reset-password", body),
            &cookie,
            Some(&csrf),
        )
    };
    let (s, _) = h
        .json(post(
            json!({"step": "totp", "email": EMAIL, "totp_code": "000000"}),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = h
        .json(post(
            json!({"step": "totp", "email": EMAIL, "totp_code": code}),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let token = v["token"].as_str().unwrap().to_string();
    let (s, _) = h
        .json(post(
            json!({"step": "password", "email": EMAIL, "token": "forged", "password": "Reset@789"}),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = h
        .json(post(
            json!({"step": "password", "email": EMAIL, "token": token, "password": "Reset@789"}),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let (s, _) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", "Reset@789")],
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn apikey_page_and_regeneration() {
    let h = H::new();
    let key = h.setup();
    let (cookie, csrf) = h.session(true);
    let mut r = with_session(get("/apikey"), &cookie, None);
    r.headers_mut()
        .insert(header::ACCEPT, "application/json".parse().unwrap());
    let (_, v) = h.json(r).await;
    assert_eq!(v["api_key"], key);
    assert_eq!(v["order_mode"], "auto");
    let (s, v) = h
        .json(with_session(
            post_json("/apikey", json!({"user_id": USER})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let new_key = v["api_key"].as_str().unwrap().to_string();
    assert_ne!(new_key, key);
    h.connect_broker();
    let (s, _) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "old key stops working at once");
    let (s, _) = h
        .json(post_json("/api/v1/ping", json!({"apikey": new_key})))
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn broker_credentials_are_masked_and_never_returned() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    let (s, v) = h
        .json(with_session(
            multipart(
                "/api/broker/credentials",
                &[
                    ("broker_api_key", "kiteapikey123"),
                    ("broker_api_secret", "supersecretvalue"),
                    ("redirect_url", "http://127.0.0.1:5000/zerodha/callback"),
                ],
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let (s, headers, body) = h
        .send(with_session(get("/api/broker/credentials"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains("supersecretvalue"));
    assert!(!text.contains("kiteapikey123"));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["data"]["broker_api_key"], "kiteap********");
    assert_eq!(v["data"]["current_broker"], "zerodha");
    assert!(headers.get(header::SET_COOKIE).is_none());
    // Broker config never exposes the key either.
    let (_, v) = h.json(get("/auth/broker-config")).await;
    assert_eq!(v["broker_name"], "zerodha");
    assert_eq!(v["broker_api_key"], Value::Null);
}

// ------------------------------------------------------------------- OAuth

fn location(headers: &axum::http::HeaderMap) -> String {
    headers[header::LOCATION].to_str().unwrap().to_string()
}

fn state_from_kite_url(url: &str) -> String {
    let u = url::Url::parse(url).unwrap();
    let rp = u
        .query_pairs()
        .find(|(k, _)| k == "redirect_params")
        .unwrap()
        .1
        .to_string();
    rp.trim_start_matches("state=").to_string()
}

#[tokio::test]
async fn zerodha_oauth_round_trip_uses_request_token_and_verifies_state() {
    let h = H::new();
    h.setup();
    h.save_broker_credentials();
    let (cookie, _) = h.session(true);

    let (s, headers, _) = h
        .send(in_app(with_session(
            get("/zerodha/initiate-oauth"),
            &cookie,
            None,
        )))
        .await;
    assert_eq!(s, StatusCode::FOUND);
    let url = location(&headers);
    assert!(url.starts_with("https://kite.zerodha.com/connect/login?v=3&api_key=kiteapikey"));
    let state = state_from_kite_url(&url);

    // Forged state: rejected, no exchange.
    let (s, headers, _) = h
        .send(get(
            "/zerodha/callback?status=success&request_token=rt1&state=forged",
        ))
        .await;
    assert_eq!(s, StatusCode::FOUND);
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(h.mock.last_auth.lock().is_none());

    // Real callback, no cookie needed (state proves the flow).
    let (s, headers, _) = h
        .send(get(&format!(
            "/zerodha/callback?status=success&action=login&request_token=rt123&state={}",
            state
        )))
        .await;
    assert_eq!(s, StatusCode::FOUND);
    assert_eq!(location(&headers), "/dashboard");
    let creds = h.mock.last_auth.lock().clone().unwrap();
    assert_eq!(creds.request_token.as_deref(), Some("rt123"));
    assert_eq!(creds.api_secret.as_deref(), Some("kitesecret"));
    assert!(h.ctx().is_broker_connected());

    // Replay of the same state is refused.
    *h.mock.last_auth.lock() = None;
    let (_, headers, _) = h
        .send(get(&format!(
            "/zerodha/callback?request_token=rt9&state={}",
            state
        )))
        .await;
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(h.mock.last_auth.lock().is_none());
}

#[tokio::test]
async fn oauth_state_expires_and_is_bound_to_the_broker() {
    let h = H::new();
    h.setup();
    h.save_broker_credentials();
    let st = {
        let url = BrokerAuthService::start_oauth(h.ctx(), "zerodha", None)
            .await
            .unwrap();
        state_from_kite_url(&url)
    };
    // Another broker's callback cannot use it.
    let (_, headers, _) = h
        .send(get(&format!("/fyers/callback?auth_code=x&state={}", st)))
        .await;
    assert!(location(&headers).starts_with("/broker?error="));

    let st2 = state_from_kite_url(
        &BrokerAuthService::start_oauth(h.ctx(), "zerodha", None)
            .await
            .unwrap(),
    );
    h.t.clock.advance(chrono::Duration::minutes(11));
    let (_, headers, _) = h
        .send(get(&format!(
            "/zerodha/callback?request_token=rt&state={}",
            st2
        )))
        .await;
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(h.mock.last_auth.lock().is_none());
}

fn aliceblue_harness(client_id: &str) -> H {
    let mock = Arc::new(MockBroker::new("aliceblue"));
    let t = build(
        BrokerRegistry::with(vec![mock.clone() as Arc<dyn crate::brokers::Broker>]),
        ist(2026, 10, 5, 10, 0),
    );
    t.ctx.limiter.freeze(Some(std::time::Instant::now()));
    let h = H { t, mock };
    h.setup();
    h.set_aliceblue_client(client_id);
    h
}

impl H {
    fn set_aliceblue_client(&self, client_id: &str) {
        let conn = self.ctx().sqlite.conn().unwrap();
        crate::db::sqlite::credentials::save(
            &conn,
            &self.ctx().security,
            "aliceblue",
            crate::db::sqlite::credentials::CredentialUpdate {
                api_key: Some("APPCODE".into()),
                api_secret: Some("absecret".into()),
                client_id: Some(client_id.into()),
                ..Default::default()
            },
        )
        .unwrap();
    }
}

const ALICE_CALLBACK: &str = "/aliceblue/callback?authCode=ac1&userId=AB1234";

#[tokio::test]
async fn aliceblue_stateless_callback_needs_the_starting_browser_session() {
    let h = aliceblue_harness("AB1234");
    let (cookie, _) = h.session(true);
    // Not started at all: refused, nothing exchanged or stored.
    let (_, headers, _) = h
        .send(with_session(get(ALICE_CALLBACK), &cookie, None))
        .await;
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(h.mock.last_auth.lock().is_none());

    let (s, headers, _) = h
        .send(with_session(
            in_app(get("/aliceblue/initiate-oauth")),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::FOUND);
    assert_eq!(
        location(&headers),
        "https://ant.aliceblueonline.com/?appcode=APPCODE"
    );

    // A forged callback: no cookie, or another browser session's cookie.
    let (_, headers, _) = h.send(get(ALICE_CALLBACK)).await;
    assert!(location(&headers).starts_with("/broker?error="));
    let (other, _) = h.session(false);
    let (_, headers, _) = h
        .send(with_session(get(ALICE_CALLBACK), &other, None))
        .await;
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(h.mock.last_auth.lock().is_none());
    assert!(!h.ctx().is_broker_connected());

    // The trader's own callback still works: the forgeries removed nothing.
    let (_, headers, _) = h
        .send(with_session(get(ALICE_CALLBACK), &cookie, None))
        .await;
    assert_eq!(location(&headers), "/dashboard");
    let creds = h.mock.last_auth.lock().clone().unwrap();
    assert_eq!(creds.auth_code.as_deref(), Some("AB1234:ac1"));
    assert!(h.ctx().is_broker_connected());

    // One start admits one callback.
    *h.mock.last_auth.lock() = None;
    let (_, headers, _) = h
        .send(with_session(get(ALICE_CALLBACK), &cookie, None))
        .await;
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(h.mock.last_auth.lock().is_none());
}

#[tokio::test]
async fn aliceblue_callback_for_another_account_is_refused() {
    // The configured client id differs from the account the broker signs in
    // (the mock answers AB1234).
    let h = aliceblue_harness("ZZ999");
    let (cookie, _) = h.session(true);
    h.send(with_session(
        in_app(get("/aliceblue/initiate-oauth")),
        &cookie,
        None,
    ))
    .await;
    let (_, headers, _) = h
        .send(with_session(get(ALICE_CALLBACK), &cookie, None))
        .await;
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(!h.ctx().is_broker_connected());
    let stored = {
        let conn = h.ctx().sqlite.conn().unwrap();
        crate::db::sqlite::auth::latest_active(&conn, &h.ctx().security).unwrap()
    };
    assert!(stored.is_none(), "a refused sign-in persists nothing");

    // The refused callback used up that pending sign-in (single use): with
    // the right client id it is still refused until the trader starts the
    // login again, and then it completes.
    h.set_aliceblue_client("AB1234");
    let (_, headers, _) = h
        .send(with_session(get(ALICE_CALLBACK), &cookie, None))
        .await;
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(!h.ctx().is_broker_connected());
    h.send(with_session(
        in_app(get("/aliceblue/initiate-oauth")),
        &cookie,
        None,
    ))
    .await;
    let (_, headers, _) = h
        .send(with_session(get(ALICE_CALLBACK), &cookie, None))
        .await;
    assert_eq!(location(&headers), "/dashboard");
    assert!(h.ctx().is_broker_connected());
    assert_eq!(h.ctx().get_broker_session().unwrap().user_id, "AB1234");

    // Without a configured client id the callback is refused, with the
    // trader told where to add it.
    let h = aliceblue_harness("");
    let (cookie, _) = h.session(true);
    h.send(with_session(
        in_app(get("/aliceblue/initiate-oauth")),
        &cookie,
        None,
    ))
    .await;
    let (_, headers, _) = h
        .send(with_session(get(ALICE_CALLBACK), &cookie, None))
        .await;
    let loc = urlencoding::decode(&location(&headers))
        .unwrap()
        .into_owned();
    assert!(
        loc.contains("Add your aliceblue client id in Profile, Broker Configuration"),
        "{}",
        loc
    );
    assert!(!h.ctx().is_broker_connected());
    assert!(h.mock.last_auth.lock().is_none());
}

#[tokio::test]
async fn samco_ip_status_without_a_samco_session_matches_the_web() {
    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    let (s, body) = h
        .json(with_session(get("/samco/ip-status"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"status": "error", "message": "Not connected to Samco. Log in to the broker first."})
    );
    let (_, headers, _) = h.send(get("/samco/callback")).await;
    assert_eq!(location(&headers), "/broker/samco/auth");
}

#[tokio::test]
async fn manual_paste_of_the_redirected_address() {
    let h = H::new();
    h.setup();
    h.save_broker_credentials();
    let (cookie, csrf) = h.session(true);
    // Started from this browser session (a pasted address completes only
    // a sign-in its own session started).
    let sid = cookie.trim_start_matches("session=").to_string();
    let st = state_from_kite_url(
        &BrokerAuthService::start_oauth(h.ctx(), "zerodha", Some(&sid))
            .await
            .unwrap(),
    );
    let pasted = format!(
        "http://127.0.0.1:5000/zerodha/callback?status=success&request_token=pasted1&state={}",
        st
    );
    let (s, v) = h
        .json(with_session(
            post_json("/auth/broker/oauth/manual", json!({"url": pasted})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["redirect"], "/dashboard");
    assert_eq!(
        h.mock
            .last_auth
            .lock()
            .clone()
            .unwrap()
            .request_token
            .as_deref(),
        Some("pasted1")
    );
}

// --------------------------------------------------------------- SPA, misc

#[tokio::test]
async fn spa_routes_and_security_headers() {
    let h = H::new();
    let (s, headers, _) = h.send(get("/dashboard")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(headers[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .contains("frame-ancestors 'none'"));
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    let (s, _, _) = h.send(get("/no-such-page")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn oversized_bodies_are_refused_with_json() {
    let h = H::new();
    let big = "x".repeat(crate::config::BODY_LIMIT_BYTES + 10);
    let r = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/ping")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!("{{\"apikey\":\"{}\"}}", big)))
        .unwrap();
    let (s, v) = h.json(r).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(v["status"], "error");
}

#[tokio::test]
async fn order_events_reach_the_log_subscriber_without_the_api_key() {
    let h = H::new();
    let meta = crate::events::OrderMeta {
        mode: crate::events::Mode::Live,
        api_type: "placeorder".into(),
        request_data: json!({"apikey": "k-secret", "symbol": "SBIN"}),
        response_data: json!({"status": "success", "orderid": "1"}),
    };
    h.ctx().bus.publish(crate::events::Event::OrderPlaced {
        meta,
        strategy: "s".into(),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        quantity: 1,
        pricetype: "MARKET".into(),
        product: "MIS".into(),
        orderid: "1".into(),
    });
    for _ in 0..100 {
        if h.ctx().logs.count_order_logs().unwrap() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let rows = h.ctx().logs.recent_order_logs(1).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].1.contains("k-secret"));
}

// ------------------------------------------------------- listener lifecycle

#[tokio::test]
async fn port_in_use_is_reported_for_the_trader_and_stop_releases_the_port() {
    let h = H::new();
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = taken.local_addr().unwrap().port();
    h.ctx().config.write().http_port = port;
    match crate::server::start(h.ctx().clone()).await {
        Err(crate::state::ServerStatus::PortInUse { port: p, message }) => {
            assert_eq!(p, port);
            assert!(message.contains(&port.to_string()));
            assert!(message.contains("already used by another program"));
        }
        Err(other) => panic!("unexpected status {:?}", other),
        Ok(_) => panic!("bound a taken port"),
    }
    assert!(matches!(
        &*h.ctx().server_status.read(),
        crate::state::ServerStatus::PortInUse { .. }
    ));
    drop(taken);

    // Free now: starts, answers over real TCP, and stopping releases the port.
    let handle = crate::server::start(h.ctx().clone()).await.unwrap();
    let mut s = tokio::net::TcpStream::connect(handle.addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    s.write_all(b"GET /auth/check-setup HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    // Report only the parsed status code, never the raw response text.
    let status: u16 = out
        .strip_prefix("HTTP/1.1 ")
        .and_then(|r| r.get(..3))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    assert!(
        status == 400 || status == 200,
        "unexpected status {}",
        status
    );
    handle.stop().await;
    let again = tokio::net::TcpListener::bind(("127.0.0.1", port)).await;
    assert!(again.is_ok(), "port released after stop");
}

#[tokio::test]
async fn shutdown_stops_owned_tasks_and_the_bus() {
    let h = H::new();
    crate::session::spawn_expiry_task(h.ctx().clone());
    assert!(h.ctx().task_count() >= 1);
    assert!(h.ctx().bus.subscriber_count() >= 2);
    h.ctx().shutdown().await;
    assert_eq!(h.ctx().task_count(), 0);
    assert_eq!(h.ctx().bus.subscriber_count(), 0);
}

#[tokio::test]
async fn tauri_commands_require_the_signed_in_user() {
    let h = H::new();
    assert!(crate::commands::require_user(h.ctx()).is_err());
    h.session(true);
    assert_eq!(crate::commands::require_user(h.ctx()).unwrap(), USER);
}

/// `/setup` is a POST-only route and a page: a browser visit or refresh
/// must get the app, not 405 (found when the dev server was first opened).
#[tokio::test]
async fn page_on_a_post_only_route_serves_the_app() {
    let h = H::new();
    let (s, _, _) = h
        .send(
            Request::builder()
                .method("GET")
                .uri("/setup")
                .header(header::ACCEPT, "text/html")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_ne!(s, StatusCode::METHOD_NOT_ALLOWED);
    // A wrong method on an API route still follows the web contract.
    let (s, _, _) = h
        .send(
            Request::builder()
                .method("GET")
                .uri("/api/v1/placeorder")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Sign-in paths per broker family (catalog-driven)
// ---------------------------------------------------------------------------

/// A context with mock adapters standing in for brokers whose sign-in
/// differs: Dhan (state-less consent redirect, pasted token), the Noren
/// pages (may drop state), tradesmart (pasted token address), rmoney (XTS
/// form-POST redirect), Kotak (form fields), jainamxts (signs in from the
/// saved keys alone).
fn family_harness() -> (TestCtx, Vec<Arc<MockBroker>>) {
    let ids = [
        "dhan",
        "shoonya",
        "tradesmart",
        "rmoney",
        "kotak",
        "aliceblue",
        "jainamxts",
    ];
    let mocks: Vec<Arc<MockBroker>> = ids.iter().map(|i| Arc::new(MockBroker::new(i))).collect();
    // Every sign-in comes back for the configured account (`U1:::appkey`).
    for m in &mocks {
        *m.auth_user_id.lock() = "U1".into();
        *m.saved_keys_sign_in.lock() =
            crate::brokers::catalog::sign_in(m.id) == crate::brokers::catalog::SignIn::SavedKeys;
    }
    let t = build(
        BrokerRegistry::with(
            mocks
                .iter()
                .map(|m| m.clone() as Arc<dyn crate::brokers::Broker>)
                .collect(),
        ),
        ist(2026, 10, 5, 10, 0),
    );
    t.ctx.limiter.freeze(Some(std::time::Instant::now()));
    AuthService::setup(&t.ctx, USER, EMAIL, PASSWORD).unwrap();
    {
        let conn = t.ctx.sqlite.conn().unwrap();
        for b in ids {
            // AliceBlue's app code does not carry the account: the Profile
            // client id names it.
            let (api_key, client_id) = match b {
                "aliceblue" => ("APPCODE", Some("U1".to_string())),
                _ => ("U1:::appkey", None),
            };
            crate::db::sqlite::credentials::save(
                &conn,
                &t.ctx.security,
                b,
                crate::db::sqlite::credentials::CredentialUpdate {
                    api_key: Some(api_key.into()),
                    api_secret: Some("appsecret".into()),
                    client_id,
                    ..Default::default()
                },
            )
            .unwrap();
        }
    }
    (t, mocks)
}

async fn send_to(
    ctx: &Arc<AppState>,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = crate::server::app(ctx.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    )
}

fn user_session(ctx: &AppState) -> (String, String, String) {
    let s = ctx.sessions.create(ctx.now());
    ctx.sessions.update(&s.id, |x| x.user = Some(USER.into()));
    (format!("session={}", s.id), s.csrf_token, s.id)
}

#[tokio::test]
async fn state_less_callbacks_are_bound_to_the_browser_session() {
    let (t, mocks) = family_harness();
    let ctx = &t.ctx;
    let (cookie, _, sid) = user_session(ctx);
    // A Noren page that drops `state`: matched to the pending sign-in this
    // browser session started, once.
    BrokerAuthService::start_oauth(ctx, "shoonya", Some(&sid))
        .await
        .unwrap();
    // Without the session cookie the callback is refused.
    let (_, headers, _) = send_to(ctx, get("/shoonya/callback?code=c1")).await;
    assert!(location(&headers).starts_with("/broker?error="));
    // Another browser session cannot use it either.
    let (other, _, _) = user_session(ctx);
    let (_, headers, _) = send_to(
        ctx,
        with_session(get("/shoonya/callback?code=c1"), &other, None),
    )
    .await;
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(mocks[1].last_auth.lock().is_none());
    let (_, headers, _) = send_to(
        ctx,
        with_session(get("/shoonya/callback?code=c1"), &cookie, None),
    )
    .await;
    assert_eq!(location(&headers), "/dashboard");
    assert_eq!(
        mocks[1]
            .last_auth
            .lock()
            .as_ref()
            .unwrap()
            .request_token
            .as_deref(),
        Some("c1")
    );
    // Used once.
    let (_, headers, _) = send_to(
        ctx,
        with_session(get("/shoonya/callback?code=c2"), &cookie, None),
    )
    .await;
    assert!(location(&headers).starts_with("/broker?error="));
    // A broker that always echoes state (zerodha-like) never falls back.
    assert!(crate::brokers::catalog::callback_carries_state("upstox"));
    ctx.runtime.teardown(ctx).await;
}

/// Arrow (and the HDFC pair) may drop `state`, and their API key does not
/// carry the account: a first state-less sign-in is refused until the
/// trader saves a client id in Profile, then it completes for that account.
#[tokio::test]
async fn first_state_less_sign_in_needs_the_saved_client_id() {
    let mock = Arc::new(MockBroker::new("arrow"));
    *mock.auth_user_id.lock() = "AR123".into();
    let t = build(
        BrokerRegistry::with(vec![mock.clone() as Arc<dyn crate::brokers::Broker>]),
        ist(2026, 10, 5, 10, 0),
    );
    let ctx = &t.ctx;
    ctx.limiter.freeze(Some(std::time::Instant::now()));
    AuthService::setup(ctx, USER, EMAIL, PASSWORD).unwrap();
    let save = |client_id: Option<&str>| {
        let conn = ctx.sqlite.conn().unwrap();
        crate::db::sqlite::credentials::save(
            &conn,
            &ctx.security,
            "arrow",
            crate::db::sqlite::credentials::CredentialUpdate {
                api_key: Some("appid".into()),
                api_secret: Some("appsecret".into()),
                client_id: client_id.map(String::from),
                ..Default::default()
            },
        )
        .unwrap();
    };
    save(None);
    let (cookie, csrf, sid) = user_session(ctx);
    BrokerAuthService::start_oauth(ctx, "arrow", Some(&sid))
        .await
        .unwrap();
    let (_, h, _) = send_to(
        ctx,
        with_session(get("/arrow/callback?request_token=r1"), &cookie, None),
    )
    .await;
    let loc = urlencoding::decode(&location(&h)).unwrap().into_owned();
    assert!(loc.starts_with("/broker?error="), "{}", loc);
    assert!(
        loc.contains("Add your arrow client id in Profile, Broker Configuration"),
        "{}",
        loc
    );
    assert!(ctx.get_broker_session().is_none());

    // Saved through the Profile form's field.
    let (s, _, v) = send_to(
        ctx,
        with_session(
            form(
                "/api/broker/credentials",
                &[
                    ("redirect_url", "http://127.0.0.1:5500/arrow/callback"),
                    ("client_id", "AR123"),
                ],
            ),
            &cookie,
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let (_, _, v) = send_to(
        ctx,
        with_session(get("/api/broker/credentials"), &cookie, None),
    )
    .await;
    assert_eq!(v["data"]["client_id"], "AR123", "{}", v);
    assert!(v["data"]["client_id_brokers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b == "arrow"));

    BrokerAuthService::start_oauth(ctx, "arrow", Some(&sid))
        .await
        .unwrap();
    let (_, h, _) = send_to(
        ctx,
        with_session(get("/arrow/callback?request_token=r2"), &cookie, None),
    )
    .await;
    assert_eq!(location(&h), "/dashboard");
    assert_eq!(ctx.get_broker_session().unwrap().user_id, "AR123");
    ctx.runtime.teardown(ctx).await;
}

/// The `state` OpenAlgo put on an authorize address (on the query, or
/// inside an encoded return address).
fn state_on(url: &str) -> String {
    let decoded = urlencoding::decode(url).unwrap().into_owned();
    decoded
        .rsplit("state=")
        .next()
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string()
}

/// Every way a broker session can be created or replaced, each tried the
/// way a forger would: no session, missing or wrong `state`, a sign-in
/// started elsewhere, another broker account. Each must be refused and
/// leave the trader's live session and its stored row untouched.
#[tokio::test]
async fn every_way_to_create_a_broker_session_refuses_a_forged_attempt() {
    const MANUAL: &str = "/auth/broker/oauth/manual";
    let cases: &[(&str, &str)] = &[
        ("GET callback, forged state", "shoonya"),
        ("GET callback, valid state, other account", "shoonya"),
        ("GET callback without state, no session cookie", "shoonya"),
        (
            "GET callback without state, other browser session",
            "shoonya",
        ),
        ("GET callback without state, other account", "shoonya"),
        ("XTS POST callback, no state", "rmoney"),
        ("XTS POST callback, forged state", "rmoney"),
        ("XTS POST callback, valid state, other account", "rmoney"),
        ("Pasted address, no CSRF token", "tradesmart"),
        ("Pasted address, cross-site", "tradesmart"),
        ("Pasted address, no pending sign-in", "tradesmart"),
        (
            "Pasted address, sign-in started by another session",
            "tradesmart",
        ),
        ("Pasted address, other account", "tradesmart"),
        ("Form login, no session", "kotak"),
        ("Form login, no CSRF token", "kotak"),
        ("Form login, cross-site", "kotak"),
        ("XTS POST callback carrying login-form fields", "rmoney"),
        ("Form login, other account", "kotak"),
        ("AliceBlue callback, forged state", "aliceblue"),
        ("AliceBlue callback, no session cookie", "aliceblue"),
        ("AliceBlue callback, other browser session", "aliceblue"),
        ("AliceBlue callback, no pending sign-in", "aliceblue"),
        ("AliceBlue callback, other account", "aliceblue"),
        (
            "AliceBlue pasted address, sign-in started by another session",
            "aliceblue",
        ),
        ("AliceBlue pasted address, other account", "aliceblue"),
        // The web signs these in on a GET of the callback; here only the
        // broker page's CSRF-checked POST does.
        ("Saved-key sign-in, GET callback", "jainamxts"),
        // Another site opens the sign-in start (or the bare callback) in the
        // trader's browser, then sends it the broker's code: had the first
        // GET recorded a pending sign-in, the state-less callback would
        // complete it.
        (
            "Sign-in started cross-site, then a state-less code",
            "shoonya",
        ),
        (
            "Bare callback opened cross-site, then a state-less code",
            "shoonya",
        ),
        ("Saved-key sign-in, no session", "jainamxts"),
        ("Saved-key sign-in, no CSRF token", "jainamxts"),
        ("Saved-key sign-in, cross-site", "jainamxts"),
    ];
    for &(name, broker) in cases {
        let (t, mocks) = family_harness();
        let ctx = &t.ctx;
        let mock = mocks.iter().find(|m| m.id == broker).unwrap().clone();
        // The trader's own live session.
        BrokerAuthService::persist(
            ctx,
            &BrokerSession {
                broker_id: broker.into(),
                auth_token: crate::security::Secret::new("own-token"),
                feed_token: None,
                user_id: "U1".into(),
                user_name: None,
                authenticated_at: ctx.now(),
            },
        )
        .unwrap();
        *mock.auth_user_id.lock() = "ATTACKER".into();
        let (cookie, csrf, sid) = user_session(ctx);
        let (other_cookie, _, other_sid) = user_session(ctx);
        let start = |s: String| async move {
            BrokerAuthService::start_oauth(ctx, broker, Some(&s))
                .await
                .unwrap()
        };
        let xts = |q: &str| {
            Request::builder()
                .method(Method::POST)
                .uri(format!("/rmoney/callback{}", q))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("sec-fetch-site", "cross-site")
                .body(Body::from("session=%7B%22token%22%3A%22t1%22%7D"))
                .unwrap()
        };
        let paste = || {
            post_json(
                MANUAL,
                json!({"url": "http://127.0.0.1:5000/tradesmart/callback?access_token=evil&uid=U1"}),
            )
        };
        let kotak = || {
            post_json(
                "/kotak/callback",
                json!({"mobile": "9999999999", "totp": "123456", "mpin": "1234"}),
            )
        };
        const ALICE: &str = "/aliceblue/callback?authCode=c&userId=U1";
        let alice_paste = || {
            post_json(
                MANUAL,
                json!({"url": format!("http://127.0.0.1:5000{}", ALICE)}),
            )
        };
        let req = match name {
            "GET callback, forged state" => {
                start(sid.clone()).await;
                with_session(get("/shoonya/callback?code=c&state=forged"), &cookie, None)
            }
            "GET callback, valid state, other account" => {
                let st = state_on(&start(sid.clone()).await);
                let q = format!("/shoonya/callback?code=c&state={}", st);
                with_session(get(&q), &cookie, None)
            }
            "GET callback without state, no session cookie" => {
                start(sid.clone()).await;
                *mock.auth_user_id.lock() = "U1".into();
                get("/shoonya/callback?code=c")
            }
            "GET callback without state, other browser session" => {
                start(sid.clone()).await;
                *mock.auth_user_id.lock() = "U1".into();
                with_session(get("/shoonya/callback?code=c"), &other_cookie, None)
            }
            "GET callback without state, other account" => {
                start(sid.clone()).await;
                with_session(get("/shoonya/callback?code=c"), &cookie, None)
            }
            "XTS POST callback, no state" => {
                start(sid.clone()).await;
                *mock.auth_user_id.lock() = "U1".into();
                xts("")
            }
            "XTS POST callback, forged state" => {
                start(sid.clone()).await;
                *mock.auth_user_id.lock() = "U1".into();
                xts("?state=forged")
            }
            "XTS POST callback, valid state, other account" => {
                let st = state_on(&start(sid.clone()).await);
                xts(&format!("?state={}", st))
            }
            "Pasted address, no CSRF token" => {
                start(sid.clone()).await;
                with_session(paste(), &cookie, None)
            }
            "Pasted address, cross-site" => {
                start(sid.clone()).await;
                let mut r = with_session(paste(), &cookie, Some(&csrf));
                r.headers_mut()
                    .insert("sec-fetch-site", "cross-site".parse().unwrap());
                r
            }
            "Pasted address, no pending sign-in" => {
                *mock.auth_user_id.lock() = "U1".into();
                with_session(paste(), &cookie, Some(&csrf))
            }
            "Pasted address, sign-in started by another session" => {
                start(other_sid.clone()).await;
                *mock.auth_user_id.lock() = "U1".into();
                with_session(paste(), &cookie, Some(&csrf))
            }
            "Pasted address, other account" => {
                start(sid.clone()).await;
                with_session(paste(), &cookie, Some(&csrf))
            }
            "Form login, no session" => {
                *mock.auth_user_id.lock() = "U1".into();
                kotak()
            }
            "Form login, no CSRF token" => {
                *mock.auth_user_id.lock() = "U1".into();
                with_session(kotak(), &cookie, None)
            }
            "Form login, cross-site" => {
                *mock.auth_user_id.lock() = "U1".into();
                let mut r = with_session(kotak(), &cookie, Some(&csrf));
                r.headers_mut()
                    .insert("sec-fetch-site", "cross-site".parse().unwrap());
                r
            }
            "XTS POST callback carrying login-form fields" => {
                *mock.auth_user_id.lock() = "U1".into();
                with_session(
                    form(
                        "/rmoney/callback",
                        &[("userid", "U1"), ("password", "p"), ("totp", "123456")],
                    ),
                    &cookie,
                    Some(&csrf),
                )
            }
            "Form login, other account" => with_session(kotak(), &cookie, Some(&csrf)),
            // AliceBlue's login page returns no `state`: only the pending
            // sign-in this browser session started, for the Profile client id.
            "AliceBlue callback, forged state" => {
                start(sid.clone()).await;
                *mock.auth_user_id.lock() = "U1".into();
                with_session(get(&format!("{}&state=forged", ALICE)), &cookie, None)
            }
            "AliceBlue callback, no session cookie" => {
                start(sid.clone()).await;
                *mock.auth_user_id.lock() = "U1".into();
                get(ALICE)
            }
            "AliceBlue callback, other browser session" => {
                start(sid.clone()).await;
                *mock.auth_user_id.lock() = "U1".into();
                with_session(get(ALICE), &other_cookie, None)
            }
            "AliceBlue callback, no pending sign-in" => {
                *mock.auth_user_id.lock() = "U1".into();
                with_session(get(ALICE), &cookie, None)
            }
            "AliceBlue callback, other account" => {
                start(sid.clone()).await;
                with_session(get(ALICE), &cookie, None)
            }
            "AliceBlue pasted address, sign-in started by another session" => {
                start(other_sid.clone()).await;
                *mock.auth_user_id.lock() = "U1".into();
                with_session(alice_paste(), &cookie, Some(&csrf))
            }
            "AliceBlue pasted address, other account" => {
                start(sid.clone()).await;
                with_session(alice_paste(), &cookie, Some(&csrf))
            }
            "Sign-in started cross-site, then a state-less code"
            | "Bare callback opened cross-site, then a state-less code" => {
                // The trader's own account: only the missing pending
                // sign-in refuses the code.
                *mock.auth_user_id.lock() = "U1".into();
                let mut path = if name.starts_with("Sign-in started") {
                    "/shoonya/initiate-oauth".to_string()
                } else {
                    "/shoonya/callback".to_string()
                };
                // The browser follows an in-app redirect to the sign-in
                // start, still a cross-site navigation.
                for _ in 0..2 {
                    let mut r = with_session(get(&path), &cookie, None);
                    r.headers_mut()
                        .insert("sec-fetch-site", "cross-site".parse().unwrap());
                    let (_, h, _) = send_to(ctx, r).await;
                    match h.get(header::LOCATION).and_then(|l| l.to_str().ok()) {
                        Some(l) if l.ends_with("/initiate-oauth") => path = l.to_string(),
                        _ => break,
                    }
                }
                let mut r = with_session(get("/shoonya/callback?code=c"), &cookie, None);
                r.headers_mut()
                    .insert("sec-fetch-site", "cross-site".parse().unwrap());
                r
            }
            // The keys are the trader's own, so the account matches: only
            // the request's own checks stand between it and a new session.
            "Saved-key sign-in, GET callback" => {
                *mock.auth_user_id.lock() = "U1".into();
                let mut r = with_session(get("/jainamxts/callback"), &cookie, None);
                r.headers_mut()
                    .insert("sec-fetch-site", "same-origin".parse().unwrap());
                r
            }
            "Saved-key sign-in, no session" => {
                *mock.auth_user_id.lock() = "U1".into();
                form("/jainamxts/callback", &[])
            }
            "Saved-key sign-in, no CSRF token" => {
                *mock.auth_user_id.lock() = "U1".into();
                with_session(form("/jainamxts/callback", &[]), &cookie, None)
            }
            "Saved-key sign-in, cross-site" => {
                *mock.auth_user_id.lock() = "U1".into();
                let mut r = with_session(form("/jainamxts/callback", &[]), &cookie, Some(&csrf));
                r.headers_mut()
                    .insert("sec-fetch-site", "cross-site".parse().unwrap());
                r
            }
            _ => unreachable!("{}", name),
        };
        let (s, h, v) = send_to(ctx, req).await;
        let loc = h
            .get(header::LOCATION)
            .and_then(|l| l.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            loc != "/dashboard" && v["status"] != "success",
            "{}: accepted ({} {} {})",
            name,
            s,
            loc,
            v
        );
        let live = ctx.get_broker_session().expect(name);
        assert_eq!(
            (
                live.broker_id.as_str(),
                live.user_id.as_str(),
                live.auth_token.expose()
            ),
            (broker, "U1", "own-token"),
            "{}: live session replaced",
            name
        );
        let conn = ctx.sqlite.conn().unwrap();
        let stored = crate::db::sqlite::auth::latest_active(&conn, &ctx.security)
            .unwrap()
            .expect(name);
        assert_eq!(
            stored.auth_token.expose(),
            "own-token",
            "{}: stored row replaced",
            name
        );
        assert_eq!(stored.user_id.as_deref(), Some("U1"), "{}", name);
        drop(conn);
        ctx.runtime.teardown(ctx).await;
    }
}

/// Login CSRF on brokers whose redirect drops `state`: an attacker page
/// sends the trader's browser (cookie attached, SameSite=Lax) to the
/// callback with the attacker's own code while a sign-in is pending.
#[tokio::test]
async fn forged_state_less_callbacks_are_refused() {
    let (t, mocks) = family_harness();
    let ctx = &t.ctx;
    let shoonya = &mocks[1];
    // Every section starts with a fresh sign-in attempt budget.
    let base = std::time::Instant::now();
    let section = std::cell::Cell::new(0u64);
    let fresh_limits = || {
        section.set(section.get() + 1);
        ctx.limiter.freeze(Some(
            base + std::time::Duration::from_secs(7_200 * section.get()),
        ));
    };
    let (cookie, _, sid) = user_session(ctx);
    let callback = |q: &str| with_session(get(&format!("/shoonya/callback?{}", q)), &cookie, None);
    let stored_user = |ctx: &AppState| {
        let c = ctx.sqlite.conn().unwrap();
        crate::db::sqlite::auth::last_user_id(&c, "shoonya").unwrap()
    };

    // The code belongs to another account: refused, nothing stored.
    BrokerAuthService::start_oauth(ctx, "shoonya", Some(&sid))
        .await
        .unwrap();
    *shoonya.auth_user_id.lock() = "ATTACKER".into();
    let (_, h, _) = send_to(ctx, callback("code=evil")).await;
    assert!(
        location(&h).starts_with("/broker?error="),
        "{}",
        location(&h)
    );
    assert!(ctx.get_broker_session().is_none());
    assert_eq!(stored_user(ctx), None);

    fresh_limits();
    // The legitimate flow still works.
    *shoonya.auth_user_id.lock() = "U1".into();
    BrokerAuthService::start_oauth(ctx, "shoonya", Some(&sid))
        .await
        .unwrap();
    let (_, h, _) = send_to(ctx, callback("code=mine")).await;
    assert_eq!(location(&h), "/dashboard");
    assert_eq!(ctx.get_broker_session().unwrap().user_id, "U1");

    fresh_limits();
    // A forged callback does not replace the connected session.
    BrokerAuthService::start_oauth(ctx, "shoonya", Some(&sid))
        .await
        .unwrap();
    *shoonya.auth_user_id.lock() = "ATTACKER".into();
    let (_, h, _) = send_to(ctx, callback("code=evil")).await;
    assert!(location(&h).starts_with("/broker?error="));
    assert_eq!(ctx.get_broker_session().unwrap().user_id, "U1");
    assert_eq!(stored_user(ctx).as_deref(), Some("U1"));
    *shoonya.auth_user_id.lock() = "U1".into();

    fresh_limits();
    // Past three minutes the state-less fallback is closed.
    BrokerAuthService::start_oauth(ctx, "shoonya", Some(&sid))
        .await
        .unwrap();
    t.clock.advance(chrono::Duration::seconds(181));
    *shoonya.last_auth.lock() = None;
    let (_, h, _) = send_to(ctx, callback("code=late")).await;
    assert!(location(&h).starts_with("/broker?error="));
    assert!(shoonya.last_auth.lock().is_none());

    fresh_limits();
    // Two sign-ins pending: only the newest completes; the older is gone.
    BrokerAuthService::start_oauth(ctx, "shoonya", Some(&sid))
        .await
        .unwrap();
    t.clock.advance(chrono::Duration::seconds(1));
    BrokerAuthService::start_oauth(ctx, "shoonya", Some(&sid))
        .await
        .unwrap();
    let (_, h, _) = send_to(ctx, callback("code=c1")).await;
    assert_eq!(location(&h), "/dashboard");
    let (_, h, _) = send_to(ctx, callback("code=c2")).await;
    assert!(location(&h).starts_with("/broker?error="));

    fresh_limits();
    // Dhan: a consent other than the one this sign-in created is refused;
    // its own consent completes.
    let dhan = &mocks[0];
    *dhan.login_url.lock() =
        Some("https://auth.dhan.co/login/consentApp-login?consentAppId=consent-1".into());
    let dhan_cb = |q: &str| with_session(get(&format!("/dhan/callback?{}", q)), &cookie, None);
    let url = BrokerAuthService::start_oauth(ctx, "dhan", Some(&sid))
        .await
        .unwrap();
    assert!(url.ends_with("consentAppId=consent-1"));
    let (_, h, _) = send_to(ctx, dhan_cb("tokenId=t9&consentAppId=consent-2")).await;
    assert!(location(&h).starts_with("/broker?error="));
    assert!(dhan.last_auth.lock().is_none());
    fresh_limits();
    // A tokenId for another Dhan account is refused too.
    BrokerAuthService::start_oauth(ctx, "dhan", Some(&sid))
        .await
        .unwrap();
    *dhan.auth_user_id.lock() = "ATTACKER".into();
    let (_, h, _) = send_to(ctx, dhan_cb("tokenId=t9")).await;
    assert!(location(&h).starts_with("/broker?error="));
    assert_eq!(ctx.get_broker_session().unwrap().broker_id, "shoonya");
    *dhan.auth_user_id.lock() = "U1".into();
    BrokerAuthService::start_oauth(ctx, "dhan", Some(&sid))
        .await
        .unwrap();
    let (_, h, _) = send_to(ctx, dhan_cb("tokenId=t1&consentAppId=consent-1")).await;
    assert_eq!(location(&h), "/dashboard");
    assert_eq!(ctx.get_broker_session().unwrap().broker_id, "dhan");
    ctx.runtime.teardown(ctx).await;
}

#[tokio::test]
async fn xts_form_post_redirect_is_public_and_state_verified() {
    let (t, mocks) = family_harness();
    let ctx = &t.ctx;
    let form = |state: &str| {
        Request::builder()
            .method(Method::POST)
            .uri(format!("/rmoney/callback?state={}", state))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("sec-fetch-site", "cross-site")
            .body(Body::from("session=%7B%22token%22%3A%22t1%22%7D"))
            .unwrap()
    };
    // A forged state is refused, with no cookie and no CSRF token needed to
    // reach the check.
    let (_, headers, _) = send_to(ctx, form("forged")).await;
    assert!(location(&headers).starts_with("/broker?error="));
    let url = BrokerAuthService::start_oauth(ctx, "rmoney", None).await;
    // The mock rmoney adapter has no authorize URL of its own; the catalogue
    // builds the XTS one with the state on the return address.
    let url = url.unwrap();
    let ret = url::Url::parse(&url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "returnURL")
        .unwrap()
        .1
        .to_string();
    let state = ret.split("state=").nth(1).unwrap().to_string();
    let (_, headers, _) = send_to(ctx, form(&state)).await;
    assert_eq!(location(&headers), "/dashboard");
    assert_eq!(
        mocks[3]
            .last_auth
            .lock()
            .as_ref()
            .unwrap()
            .request_token
            .as_deref(),
        Some("{\"token\":\"t1\"}")
    );
    // For any other broker the POST stays the signed-in login form.
    let (s, _, _) = send_to(
        ctx,
        Request::builder()
            .method(Method::POST)
            .uri("/kotak/callback")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    // No session, no CSRF token: refused before the form is read.
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(mocks[4].last_auth.lock().is_none());
    ctx.runtime.teardown(ctx).await;
}

#[tokio::test]
async fn login_forms_follow_the_brokers_own_fields() {
    let (t, mocks) = family_harness();
    let ctx = &t.ctx;
    let (cookie, csrf, _) = user_session(ctx);
    // Kotak: mobile, TOTP and MPIN are required.
    let (s, _, v) = send_to(
        ctx,
        with_session(
            post_json(
                "/kotak/callback",
                json!({"mobile": "9999999999", "totp": "123456"}),
            ),
            &cookie,
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(v["message"], "Enter the MPIN to sign in.");
    let (s, _, v) = send_to(
        ctx,
        with_session(
            post_json(
                "/kotak/callback",
                json!({"mobile": "9999999999", "totp": "123456", "mpin": "1234"}),
            ),
            &cookie,
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let a = mocks[4].last_auth.lock().clone().unwrap();
    assert_eq!(a.client_id.as_deref(), Some("9999999999"));
    assert_eq!(a.password.as_deref(), Some("1234"));
    assert_eq!(a.totp.as_deref(), Some("123456"));
    // Dhan signs in by redirect, but a pasted access token is accepted by
    // the form (web brlogin `access_token`).
    let (s, _, v) = send_to(
        ctx,
        with_session(
            post_json("/dhan/callback", json!({"access_token": "pasted-token"})),
            &cookie,
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        mocks[0]
            .last_auth
            .lock()
            .as_ref()
            .unwrap()
            .password
            .as_deref(),
        Some("pasted-token")
    );
    ctx.runtime.teardown(ctx).await;
}

#[tokio::test]
async fn pasted_token_addresses_need_the_signed_in_trader() {
    let (t, mocks) = family_harness();
    let ctx = &t.ctx;
    // Arriving by redirect, a token address is not trusted on its own.
    let (_, headers, _) = send_to(ctx, get("/tradesmart/callback?access_token=tok&uid=U1")).await;
    assert!(location(&headers).starts_with("/broker?error="));
    assert!(mocks[2].last_auth.lock().is_none());
    // Pasted by the signed-in trader, after starting the sign-in in this
    // session, it is used as is.
    let (cookie, csrf, sid) = user_session(ctx);
    BrokerAuthService::start_oauth(ctx, "tradesmart", Some(&sid))
        .await
        .unwrap();
    let (s, _, v) = send_to(
        ctx,
        with_session(
            post_json(
                "/auth/broker/oauth/manual",
                json!({"url": "http://127.0.0.1:5000/tradesmart/callback?access_token=tok&uid=U1"}),
            ),
            &cookie,
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let a = mocks[2].last_auth.lock().clone().unwrap();
    assert_eq!(a.password.as_deref(), Some("tok"));
    assert_eq!(a.client_id.as_deref(), Some("U1"));
    assert!(a.request_token.is_none());
    ctx.runtime.teardown(ctx).await;
}

#[tokio::test]
async fn master_contract_routes_and_server_settings_status() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    // No broker session: the web's 401.
    let (s, v) = h
        .json(with_session(
            get("/api/master-contract/status"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        v,
        json!({"status": "error", "message": "No broker session found"})
    );
    h.ctx().set_broker_session(Some(BrokerSession {
        broker_id: "zerodha".into(),
        auth_token: crate::security::Secret::new("mock-access-token"),
        feed_token: None,
        user_id: "AB1234".into(),
        user_name: None,
        authenticated_at: h.ctx().now(),
    }));
    let (s, v) = h
        .json(with_session(
            get("/api/master-contract/status"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "unknown");
    assert_eq!(v["total_symbols"], "0");
    let (_, v) = h
        .json(with_session(
            get("/api/master-contract/smart-status"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(
        v["smart_download"],
        json!({"should_download": true, "reason": "No previous download found",
               "cutoff_time": "08:00", "cutoff_timezone": "IST"})
    );
    let (_, v) = h
        .json(with_session(get("/api/cache/health"), &cookie, None))
        .await;
    assert_eq!(v["health_score"], 0);
    assert_eq!(v["status"], "unhealthy");
    // Forced download, then the status, the cache and the smart rule agree.
    *h.mock.master.lock() = Some(Ok(vec![crate::brokers::common::symbols::tests::row(
        "SBIN", "SBIN-EQ", "NSE", "3045",
    )]));
    let (s, v) = h
        .json(with_session(
            post_json("/api/master-contract/download", json!({"force": true})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["started"], true);
    for _ in 0..200 {
        if h.ctx().symbol_count() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let (_, v) = h
        .json(with_session(
            get("/api/master-contract/status"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(v["status"], "success");
    assert_eq!(v["is_ready"], true);
    assert_eq!(v["total_symbols"], "1");
    let (_, v) = h
        .json(with_session(
            post_json("/api/master-contract/download", json!({})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(v["status"], "skipped");
    assert_eq!(v["should_download"], false);
    let (s, v) = h
        .json(with_session(
            post_json("/api/cache/reload", json!({})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["message"],
        "Cache reloaded successfully for broker: zerodha"
    );
    let (_, v) = h
        .json(with_session(get("/api/cache/health"), &cookie, None))
        .await;
    assert_eq!(v["health_score"], 100);

    // Web #2117: a forced download while one runs is refused with the web's
    // sentence, and the running download's status row is left alone.
    assert!(h.ctx().runtime.claims.claim("zerodha"));
    let (s, v) = h
        .json(with_session(
            post_json("/api/master-contract/download", json!({"force": true})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert_eq!(
        v,
        json!({"status": "error", "started": false,
               "message": "A master contract download is already running. Wait for it to finish, then try again."})
    );
    let (_, v) = h
        .json(with_session(
            get("/api/master-contract/status"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(v["status"], "success");
    h.ctx().runtime.claims.release("zerodha");

    // The feed listener's state reaches Server Settings with its fix.
    *h.ctx().feed_status.write() = crate::state::ServerStatus::PortInUse {
        port: 8766,
        message: "Port 8766 is already used by another program.".into(),
    };
    let (_, v) = h
        .json(with_session(get("/settings/api/server"), &cookie, None))
        .await;
    assert_eq!(
        v["data"]["ws_status"],
        json!({"state": "port_in_use", "port": 8766,
               "message": "Port 8766 is already used by another program."})
    );
    let st = crate::commands::startup_status_of(h.ctx());
    let sv = serde_json::to_value(&st).unwrap();
    assert_eq!(sv["ws"]["state"], "port_in_use");
    assert!(sv.get("state").is_some());
    h.ctx().runtime.teardown(h.ctx()).await;
}
