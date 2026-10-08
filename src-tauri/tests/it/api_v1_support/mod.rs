//! Harness for the `/api/v1` integration tests: a real app context (temp
//! data dir, memory keystore, manual clock at the fixture session's time)
//! with the scriptable `MockBroker` as the connected broker, a symbol master
//! and quote table built from the golden fixtures, the full router driven
//! in-process, and an event recorder on the real bus.

#![allow(dead_code)]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::mock::MockBroker;
use openalgo_desktop_lib::brokers::types::Quote;
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::events::{Event, Lane, Subscriber, Topic};
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use openalgo_desktop_lib::services::auth_service::AuthService;
use openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService;
use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/web/rest")
}

pub fn fixture(rel: &str) -> Value {
    let p = fixtures_root().join(rel);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", rel, e)))
        .unwrap_or_else(|e| panic!("{}: {}", rel, e))
}

/// Every fixture file, as `endpoint/case.json`, sorted.
pub fn all_fixtures() -> Vec<String> {
    let mut out = Vec::new();
    let root = fixtures_root();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().map(|x| x == "json").unwrap_or(false) {
                out.push(
                    p.strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    out.sort();
    out
}

/// 2026-10-03 09:42 IST, the time session 2 was recorded.
pub fn session_time() -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(2026, 10, 3, 9, 42, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

fn sym(
    symbol: &str,
    exchange: &str,
    name: &str,
    lot: i32,
    expiry: &str,
    strike: f64,
    it: &str,
) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: name.into(),
        exchange: exchange.into(),
        brexchange: if exchange.ends_with("_INDEX") {
            exchange.trim_end_matches("_INDEX").into()
        } else {
            exchange.into()
        },
        token: format!("{}:{}", exchange, symbol),
        expiry: expiry.into(),
        strike,
        lot_size: lot,
        instrument_type: it.into(),
        tick_size: 0.05,
    }
}

fn row_from(v: &Value) -> SymToken {
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    SymToken {
        symbol: s("symbol"),
        brsymbol: s("brsymbol"),
        name: s("name"),
        exchange: s("exchange"),
        brexchange: s("brexchange"),
        token: s("token"),
        expiry: s("expiry"),
        strike: v.get("strike").and_then(Value::as_f64).unwrap_or(0.0),
        lot_size: v.get("lotsize").and_then(Value::as_i64).unwrap_or(1) as i32,
        instrument_type: s("instrumenttype"),
        tick_size: v.get("tick_size").and_then(Value::as_f64).unwrap_or(0.05),
    }
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

fn compact(expiry: &str) -> String {
    expiry.replace('-', "")
}

/// The symbol master the fixtures need: recorded rows first (instruments,
/// symbol, search fixtures, in that order), then synthesized F&O chains
/// for every expiry the expiry fixtures list.
pub fn master() -> Vec<SymToken> {
    let mut rows: Vec<SymToken> = Vec::new();
    for rel in ["instruments/nse_json.json", "instruments/nfo_json.json"] {
        for r in fixture(rel)["response"]["body"]["data"].as_array().unwrap() {
            rows.push(row_from(r));
        }
    }
    for rel in all_fixtures() {
        if rel.starts_with("symbol/") || rel.starts_with("search/") {
            let b = &fixture(&rel)["response"]["body"]["data"];
            match b {
                Value::Array(a) => rows.extend(a.iter().map(row_from)),
                Value::Object(_) => rows.push(row_from(b)),
                _ => {}
            }
        }
    }
    for s in ["RELIANCE", "SBIN", "INFY", "TCS", "HDFCBANK", "ITC"] {
        rows.push(sym(s, "NSE", s, 1, "", 0.0, "EQ"));
        rows.push(sym(s, "BSE", s, 1, "", 0.0, "EQ"));
    }
    rows.push(sym("NIFTY", "NSE_INDEX", "NIFTY 50", 0, "", 0.0, "EQ"));
    rows.push(sym(
        "BANKNIFTY",
        "NSE_INDEX",
        "NIFTY BANK",
        0,
        "",
        0.0,
        "EQ",
    ));
    // Futures and options per the expiry fixtures.
    let expiries = |rel: &str| -> Vec<String> {
        fixture(rel)["response"]["body"]["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    for e in expiries("expiry/nifty_nfo_futures.json") {
        rows.push(sym(
            &format!("NIFTY{}FUT", compact(&e)),
            "NFO",
            "NIFTY",
            65,
            &e,
            0.0,
            "FUT",
        ));
    }
    for e in expiries("expiry/crudeoil_mcx_futures.json") {
        rows.push(sym(
            &format!("CRUDEOIL{}FUT", compact(&e)),
            "MCX",
            "CRUDEOIL",
            100,
            &e,
            0.0,
            "FUT",
        ));
    }
    for e in expiries("expiry/nifty_nfo_options.json") {
        let strikes: Vec<f64> = if e == "06-OCT-26" || e == "27-OCT-26" {
            (0..=24).map(|i| 21800.0 + 50.0 * i as f64).collect()
        } else {
            vec![22400.0]
        };
        for k in strikes {
            for t in ["CE", "PE"] {
                rows.push(sym(
                    &format!("NIFTY{}{}{}", compact(&e), k as i64, t),
                    "NFO",
                    "NIFTY",
                    65,
                    &e,
                    k,
                    t,
                ));
            }
        }
    }
    for e in expiries("expiry/banknifty_nfo_options.json") {
        let strikes: Vec<f64> = if e == "27-OCT-26" {
            (0..=20).map(|i| 53500.0 + 100.0 * i as f64).collect()
        } else {
            vec![54500.0]
        };
        for k in strikes {
            for t in ["CE", "PE"] {
                rows.push(sym(
                    &format!("BANKNIFTY{}{}{}", compact(&e), k as i64, t),
                    "NFO",
                    "BANKNIFTY",
                    30,
                    &e,
                    k,
                    t,
                ));
            }
        }
    }
    let _ = MONTHS;
    rows
}

fn quote_from(symbol: &str, exchange: &str, d: &Value) -> Quote {
    let f = |k: &str| d.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let i = |k: &str| d.get(k).and_then(Value::as_f64).unwrap_or(0.0) as i64;
    Quote {
        symbol: symbol.into(),
        exchange: exchange.into(),
        ltp: f("ltp"),
        open: f("open"),
        high: f("high"),
        low: f("low"),
        close: f("prev_close"),
        volume: i("volume"),
        bid: f("bid"),
        ask: f("ask"),
        bid_qty: i("bid_qty"),
        ask_qty: i("ask_qty"),
        oi: i("oi"),
        ..Default::default()
    }
}

/// Quotes recorded in the fixtures (quotes, multiquotes, option chain
/// legs), plus a plain LTP for every other master row.
pub fn quotes(rows: &[SymToken]) -> Vec<Quote> {
    let mut out: Vec<Quote> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push = |q: Quote, out: &mut Vec<Quote>| {
        if seen.insert((q.exchange.clone(), q.symbol.clone())) {
            out.push(q);
        }
    };
    for rel in all_fixtures() {
        let f = fixture(&rel);
        let Some(req) = f.get("request") else {
            continue;
        };
        let body = &f["response"]["body"];
        if rel.starts_with("quotes/") && f["response"]["status_code"] == 200 {
            let rb = &req["body"];
            push(
                quote_from(
                    rb["symbol"].as_str().unwrap(),
                    rb["exchange"].as_str().unwrap(),
                    &body["data"],
                ),
                &mut out,
            );
        }
        if rel.starts_with("multiquotes/") {
            for r in body["results"].as_array().into_iter().flatten() {
                if r.get("data").is_some() {
                    push(
                        quote_from(
                            r["symbol"].as_str().unwrap(),
                            r["exchange"].as_str().unwrap(),
                            &r["data"],
                        ),
                        &mut out,
                    );
                }
            }
        }
        if rel.starts_with("optionchain/") {
            for row in body["chain"].as_array().into_iter().flatten() {
                for side in ["ce", "pe"] {
                    if let Some(leg) = row.get(side).filter(|l| l.is_object()) {
                        push(
                            quote_from(leg["symbol"].as_str().unwrap(), "NFO", leg),
                            &mut out,
                        );
                    }
                }
            }
        }
    }
    for r in rows {
        let ltp = match r.instrument_type.as_str() {
            "CE" | "PE" => 100.0,
            "FUT" if r.exchange == "MCX" => 9000.0,
            "FUT" => 22500.0,
            _ if r.exchange.ends_with("_INDEX") => 22000.0,
            _ => 1000.0,
        };
        push(
            Quote {
                symbol: r.symbol.clone(),
                exchange: r.exchange.clone(),
                ltp,
                close: ltp,
                ..Default::default()
            },
            &mut out,
        );
    }
    out
}

/// Records every event published on the bus.
#[derive(Default)]
pub struct Recorder {
    pub events: Mutex<Vec<Arc<Event>>>,
}

#[async_trait::async_trait]
impl Subscriber for Recorder {
    fn name(&self) -> &'static str {
        "test-recorder"
    }
    fn topics(&self) -> Vec<Topic> {
        vec![
            Topic::OrderPlaced,
            Topic::OrderFailed,
            Topic::OrderNoAction,
            Topic::OrderModified,
            Topic::OrderModifyFailed,
            Topic::OrderCancelled,
            Topic::OrderCancelFailed,
            Topic::AllOrdersCancelled,
            Topic::PositionClosed,
            Topic::BasketCompleted,
            Topic::SplitCompleted,
            Topic::OptionsCompleted,
            Topic::MultiOrderCompleted,
            Topic::AnalyzerError,
            Topic::GttPlaced,
            Topic::GttFailed,
            Topic::GttModified,
            Topic::GttModifyFailed,
            Topic::GttCancelled,
            Topic::GttCancelFailed,
        ]
    }
    async fn handle(&self, event: Arc<Event>) {
        self.events.lock().push(event);
    }
}

impl Recorder {
    pub fn topics(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .iter()
            .map(|e| e.topic().as_str())
            .collect()
    }

    pub fn clear(&self) {
        self.events.lock().clear();
    }

    /// Wait until `n` events arrived (the bus delivers on worker tasks).
    pub async fn wait_for(&self, n: usize) -> Vec<Arc<Event>> {
        for _ in 0..200 {
            if self.events.lock().len() >= n {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        self.events.lock().clone()
    }
}

pub struct H {
    pub ctx: Arc<AppState>,
    pub mock: Arc<MockBroker>,
    pub clock: Arc<ManualClock>,
    pub key: String,
    pub recorder: Arc<Recorder>,
    ip: AtomicU32,
    _dir: tempfile::TempDir,
}

impl H {
    /// A signed-in user with an API key and the mock broker connected.
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let symbols = SymbolResolver::new();
        let mock = Arc::new(MockBroker::with_symbols("zerodha", symbols.clone()));
        let clock = ManualClock::new(session_time());
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: clock.clone(),
                brokers: Arc::new(BrokerRegistry::with_symbols(
                    symbols,
                    vec![mock.clone() as Arc<dyn Broker>],
                )),
            },
        )
        .unwrap();
        let rows = master();
        for q in quotes(&rows) {
            mock.set_quote(q);
        }
        ctx.load_symbol_cache(rows);
        AuthService::setup(&ctx, "trader", "trader@example.com", "Secret@123").unwrap();
        let key = ApiKeyService::current(&ctx)
            .unwrap()
            .unwrap()
            .expose()
            .to_string();
        BrokerAuthService::persist(
            &ctx,
            &BrokerSession {
                broker_id: "zerodha".into(),
                auth_token: "mock-access-token".into(),
                feed_token: None,
                user_id: "AB1234".into(),
                user_name: None,
                authenticated_at: ctx.now(),
            },
        )
        .unwrap();
        let recorder = Arc::new(Recorder::default());
        ctx.bus.subscribe(recorder.clone(), Lane::Critical);
        H {
            ctx,
            mock,
            clock,
            key,
            recorder,
            ip: AtomicU32::new(1),
            _dir: dir,
        }
    }

    pub fn analyze(&self, on: bool) {
        self.ctx.sqlite.set_analyze_mode(on).unwrap();
    }

    /// A fresh client address per request so the per-IP limits never
    /// interfere with a replay.
    pub fn next_ip(&self) -> IpAddr {
        let n = self.ip.fetch_add(1, Ordering::Relaxed);
        IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + n))
    }

    pub async fn send_from(
        &self,
        mut req: Request<Body>,
        ip: IpAddr,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip, 40000)));
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
        let ip = self.next_ip();
        self.send_from(req, ip).await
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let (s, _, b) = self.send(post_json(path, body)).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    /// `body` with the API key filled in.
    pub fn with_key(&self, mut body: Value) -> Value {
        if let Some(m) = body.as_object_mut() {
            m.insert("apikey".into(), json!(self.key));
        }
        body
    }

    pub async fn shutdown(self) {
        self.ctx.shutdown().await;
    }
}

pub fn post_json(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

thread_local! {
    /// When set, a list the fixture has entries in must not come back empty.
    pub static STRICT_ARRAYS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Same JSON shape: same object keys and value types, recursively (first
/// element of arrays when both are non-empty). Fixture bookkeeping keys
/// (`_total_rows`, `_truncated`) are ignored.
pub fn same_shape(want: &Value, got: &Value) -> Result<(), String> {
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => {
            let wk: BTreeSet<&String> = w.keys().filter(|k| !k.starts_with('_')).collect();
            let gk: BTreeSet<&String> = g.keys().collect();
            if wk != gk {
                return Err(format!("keys differ: want {:?}, got {:?}", wk, gk));
            }
            for k in wk {
                same_shape(&w[k], &g[k]).map_err(|e| format!("{}.{}", k, e))?;
            }
            Ok(())
        }
        (Value::Array(w), Value::Array(g)) => match (w.first(), g.first()) {
            (Some(a), Some(b)) => same_shape(a, b).map_err(|e| format!("[0]{}", e)),
            (Some(_), None) if STRICT_ARRAYS.with(|c| c.get()) => {
                Err(": want a non-empty list, got an empty one".to_string())
            }
            _ => Ok(()),
        },
        (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_))
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Null, Value::Null) => Ok(()),
        _ => Err(format!(": type differs: want {}, got {}", want, got)),
    }
}
