//! Nuvama (API Connect) end to end against a local fake Nuvama on an
//! ephemeral port (web `broker/nuvama`, openalgo ee123273b): sign-in,
//! headers, order bodies and paths, books, funds, quotes, history, the
//! master download and placement outcomes (`rest`), the loopback line
//! relay to a fake streaming socket (`relay`), descriptors across relay
//! reconnects (`hygiene`), and credentials that never reach a log
//! (`secrets`).

mod hygiene;
mod relay;
mod rest;
mod secrets;

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::families::noren::zip;
use openalgo_desktop_lib::brokers::nuvama::master_contract::parse_master;
use openalgo_desktop_lib::brokers::nuvama::{NuvamaBroker, NuvamaUrls};
use openalgo_desktop_lib::brokers::types::AuthToken;
use parking_lot::Mutex;
use serde_json::Value;
use std::sync::Arc;

pub const CSV: &str = include_str!("../../fixtures/brokers/nuvama/master.csv");
const RESPONSES: &str = include_str!("../../fixtures/brokers/nuvama/responses.json");

/// The session every REST test signs with.
pub const TOKEN: &str = "AUTHV:::SRCTOK:::APPKEY3:::ACC1:::EQ:::KEY1";

pub fn auth() -> AuthToken {
    AuthToken::new(TOKEN)
}

pub fn fixture(key: &str) -> Value {
    let all: Value = serde_json::from_str(RESPONSES).unwrap();
    all[key].clone()
}

pub fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_master(CSV).unwrap());
    r
}

/// One request the fake saw.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Value,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }
}

/// An answer for requests whose path ends with `path`, before the default.
#[derive(Clone)]
pub struct Script {
    pub method: &'static str,
    pub path: String,
    pub status: u16,
    pub body: String,
}

#[derive(Default)]
pub struct Fake {
    pub seen: Mutex<Vec<Seen>>,
    pub scripts: Mutex<Vec<Script>>,
}

impl Fake {
    pub fn calls(&self, method: &str, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.method == method && s.path == path)
            .cloned()
            .collect()
    }

    /// Answer `method path` with `status` and `body` from now on.
    pub fn script(&self, method: &'static str, path: &str, status: u16, body: Value) {
        let mut s = self.scripts.lock();
        s.retain(|x| !(x.method == method && x.path == path));
        s.push(Script {
            method,
            path: path.into(),
            status,
            body: body.to_string(),
        });
    }
}

fn json_reply(status: u16, body: String, app_id_key: Option<&str>) -> Response {
    let mut r = (
        StatusCode::from_u16(status).unwrap(),
        [("content-type", "application/json")],
        body,
    )
        .into_response();
    if let Some(k) = app_id_key {
        r.headers_mut().insert("AppIdKey", k.parse().unwrap());
    }
    r
}

/// The fake's default answer for `method path`.
fn default_answer(method: &Method, path: &str) -> Response {
    let ok = |key: &str| json_reply(200, fixture(key).to_string(), None);
    match (method.as_str(), path) {
        ("POST", p) if p.starts_with("/edelmw-login/login/accounts/loginvendor/") => {
            json_reply(200, fixture("login_vendor").to_string(), Some("APPKEY2"))
        }
        ("POST", "/edelmw-login/login/accounts/logindata/") => {
            json_reply(200, fixture("login_data").to_string(), Some("APPKEY3"))
        }
        ("GET", "/edelmw-eq/eq/order/book/ACC1/v1/") => ok("order_book"),
        ("GET", "/edelmw-eq/eq/tradebook/v1/ACC1/") => ok("trade_book"),
        ("GET", "/edelmw-eq/eq/positions/net/ACC1/") => ok("positions"),
        ("GET", "/edelmw-eq/eq/holdings/v1/rmsholdings/ACC1/") => ok("holdings"),
        ("GET", "/edelmw-eq/eq/limits/rmssublimits/ACC1/") => ok("funds"),
        ("GET", p) if p.starts_with("/edelmw-content/content/quote/scrip/") => ok("quote"),
        ("POST", "/edelmw-eq/eq/trade/placetrade/v1/ACC1/") => ok("place_ok"),
        ("PUT", "/edelmw-eq/eq/trade/modifytrade/v1/ACC1/") => {
            json_reply(200, r#"{"data":{"msg":"modified"}}"#.into(), None)
        }
        ("PUT", "/edelmw-eq/eq/trade/canceltrade/v1/ACC1/") => {
            json_reply(200, r#"{"data":{"msg":"cancelled"}}"#.into(), None)
        }
        ("POST", p) if p.starts_with("/edelmw-content/content/charts/v2/main/") => {
            ok("chart_intraday")
        }
        ("GET", "/app/toccontracts/instruments.zip") => (
            StatusCode::OK,
            zip::build("instruments.csv", CSV.as_bytes(), true),
        )
            .into_response(),
        _ => json_reply(404, "{}".into(), None),
    }
}

/// A fake Nuvama on an ephemeral port; every request is recorded.
pub async fn serve() -> (Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let f = fake.clone();
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let f = f.clone();
            async move {
                let path = uri.path().to_string();
                f.seen.lock().push(Seen {
                    method: method.to_string(),
                    path: path.clone(),
                    headers,
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                });
                let scripted = f
                    .scripts
                    .lock()
                    .iter()
                    .find(|s| s.method == method.as_str() && path.ends_with(&s.path))
                    .cloned();
                match scripted {
                    Some(s) => json_reply(s.status, s.body, None),
                    None => default_answer(&method, &path),
                }
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (fake, format!("http://{}", addr))
}

/// The adapter against the fake, with the fixture master loaded.
pub async fn broker() -> (Arc<Fake>, NuvamaBroker) {
    let (fake, base) = serve().await;
    let b = NuvamaBroker::with_base_url(master(), NuvamaUrls::rebased(&base, 1));
    (fake, b)
}
