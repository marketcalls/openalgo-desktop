//! Options and portfolio tools: every route the tool pages call (OI
//! tracker, max pain, OI profile, IV chart, gamma density, straddles,
//! vol surface, GEX, Strike Profile, IV smile, arbitrage, Strategy Builder
//! charts and the strategy portfolio), driven in-process against the full
//! router with the mock broker connected; plus the bounded history fan-out.
//!
//! Self-contained (no shared support module) so it can be folded into the
//! consolidated integration crate unchanged.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::mock::{MockBroker, MockCall};
use openalgo_desktop_lib::brokers::types::{Candle, Quote};
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::services::auth_service::AuthService;
use openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService;
use openalgo_desktop_lib::services::tools_service::{fan_out, Gate};
use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const USER: &str = "trader";
const STRIKES: [f64; 5] = [24_800.0, 24_900.0, 25_000.0, 25_100.0, 25_200.0];
const SPOT: f64 = 25_010.0;

fn ist(d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(2026, 10, d, h, mi, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

fn sym(symbol: &str, exchange: &str, name: &str, expiry: &str, strike: f64, it: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: name.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: format!("{}:{}", exchange, symbol),
        expiry: expiry.into(),
        strike,
        lot_size: if exchange == "NSE_INDEX" { 1 } else { 75 },
        instrument_type: it.into(),
        tick_size: 0.05,
    }
}

fn master() -> Vec<SymToken> {
    let mut v = vec![
        sym("NIFTY", "NSE_INDEX", "NIFTY 50", "", 0.0, "INDEX"),
        sym("NIFTY30OCT26FUT", "NFO", "NIFTY", "30-OCT-26", 0.0, "FUT"),
        sym("NIFTY27NOV26FUT", "NFO", "NIFTY", "27-NOV-26", 0.0, "FUT"),
        sym("GOLD05DEC26FUT", "MCX", "GOLD", "05-DEC-26", 0.0, "FUT"),
        sym("GOLD05FEB27FUT", "MCX", "GOLD", "05-FEB-27", 0.0, "FUT"),
    ];
    for (code, db) in [("30OCT26", "30-OCT-26"), ("27NOV26", "27-NOV-26")] {
        for k in STRIKES {
            for t in ["CE", "PE"] {
                v.push(sym(
                    &format!("NIFTY{}{}{}", code, k as i64, t),
                    "NFO",
                    "NIFTY",
                    db,
                    k,
                    t,
                ));
            }
        }
    }
    v
}

fn ce_oi(i: usize) -> i64 {
    1000 * (i as i64 + 1)
}

fn pe_oi(i: usize) -> i64 {
    1000 * (5 - i as i64)
}

fn quote(symbol: &str, exchange: &str, ltp: f64, oi: i64) -> Quote {
    Quote {
        symbol: symbol.into(),
        exchange: exchange.into(),
        ltp,
        close: ltp,
        oi,
        volume: oi * 3,
        ..Default::default()
    }
}

struct H {
    ctx: Arc<AppState>,
    mock: Arc<MockBroker>,
    cookie: String,
    csrf: String,
    _dir: tempfile::TempDir,
}

impl H {
    fn new(broker: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let symbols = SymbolResolver::new();
        let mock = Arc::new(MockBroker::with_symbols("zerodha", symbols.clone()));
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: ManualClock::new(ist(5, 10, 0)),
                brokers: Arc::new(BrokerRegistry::with_symbols(
                    symbols,
                    vec![mock.clone() as Arc<dyn Broker>],
                )),
            },
        )
        .unwrap();
        ctx.load_symbol_cache(master());
        mock.set_quote(quote("NIFTY", "NSE_INDEX", SPOT, 0));
        mock.set_quote(quote("NIFTY30OCT26FUT", "NFO", 25_060.0, 0));
        for code in ["30OCT26", "27NOV26"] {
            for (i, k) in STRIKES.iter().enumerate() {
                let ce = (SPOT - k).max(0.0) + 120.0;
                let pe = (k - SPOT).max(0.0) + 110.0;
                mock.set_quote(quote(
                    &format!("NIFTY{}{}CE", code, *k as i64),
                    "NFO",
                    ce,
                    ce_oi(i),
                ));
                mock.set_quote(quote(
                    &format!("NIFTY{}{}PE", code, *k as i64),
                    "NFO",
                    pe,
                    pe_oi(i),
                ));
            }
        }
        // The same candles for every history call: Friday's close, then
        // Monday's first three minutes.
        let candle = |t: DateTime<Utc>, close: f64, oi: i64| Candle {
            timestamp: t.timestamp(),
            open: close,
            high: close,
            low: close,
            close,
            volume: 10,
            oi,
        };
        *mock.history.lock() = Some(Ok(vec![
            candle(ist(2, 15, 29), 24_990.0, 400),
            candle(ist(5, 9, 15), SPOT, 500),
            candle(ist(5, 9, 16), 25_020.0, 500),
            candle(ist(5, 9, 17), 25_030.0, 600),
        ]));
        AuthService::setup(&ctx, USER, "trader@example.com", "Secret@123").unwrap();
        if broker {
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
        }
        let s = ctx.sessions.create(ctx.now());
        ctx.sessions.update(&s.id, |x| x.user = Some(USER.into()));
        H {
            ctx,
            mock,
            cookie: format!("session={}", s.id),
            csrf: s.csrf_token,
            _dir: dir,
        }
    }

    async fn raw(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        csrf: bool,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ACCEPT, "application/json")
            .header(header::COOKIE, &self.cookie);
        if csrf {
            b = b.header("x-csrftoken", &self.csrf);
        }
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let mut req = b.body(body).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
        crate::with_host(&mut req, &self.ctx);
        let resp = openalgo_desktop_lib::server::app(self.ctx.clone())
            .oneshot(req)
            .await
            .unwrap();
        let s = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.raw(Method::POST, path, Some(body), true).await
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.raw(Method::GET, path, None, false).await
    }

    fn calls(&self, f: impl Fn(&MockCall) -> bool) -> usize {
        self.mock.calls().iter().filter(|c| f(c)).count()
    }
}

fn nifty() -> Value {
    json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_date": "30OCT26"})
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

// ------------------------------------------------------------- access

const ROUTES: &[(&str, &str)] = &[
    ("POST", "/oiprofile/api/profile-data"),
    ("GET", "/oiprofile/api/intervals"),
    ("POST", "/oitracker/api/oi-data"),
    ("POST", "/oitracker/api/maxpain"),
    ("POST", "/ivchart/api/iv-data"),
    ("POST", "/ivchart/api/default-symbols"),
    ("GET", "/ivchart/api/intervals"),
    ("POST", "/gammadensity/api/gamma-data"),
    ("POST", "/straddle/api/straddle-data"),
    ("GET", "/straddle/api/intervals"),
    ("POST", "/straddlepnl/api/simulate"),
    ("GET", "/straddlepnl/api/lotsize"),
    ("GET", "/straddlepnl/api/intervals"),
    ("POST", "/volsurface/api/surface-data"),
    ("POST", "/gex/api/gex-data"),
    ("POST", "/strikeprofile/api/profile-data"),
    ("POST", "/ivsmile/api/iv-smile-data"),
    ("GET", "/arbitrage/api/universe"),
    ("POST", "/strategybuilder/api/strategy-chart"),
    ("POST", "/strategybuilder/api/multi-strike-oi"),
    ("GET", "/strategybuilder/api/intervals"),
    ("GET", "/api/strategy-portfolio"),
    ("POST", "/api/strategy-portfolio"),
    ("GET", "/api/strategy-portfolio/1"),
    ("PUT", "/api/strategy-portfolio/1"),
    ("DELETE", "/api/strategy-portfolio/1"),
];

#[test]
fn every_tool_route_is_declared_for_the_signed_in_user() {
    use openalgo_desktop_lib::server::routes::{table, Access};
    let t = table();
    for (m, p) in ROUTES {
        let path = p.replace("/1", "/{id}");
        let spec = t
            .iter()
            .find(|s| s.method.as_str() == *m && s.path == path)
            .unwrap_or_else(|| panic!("{} {} missing", m, p));
        assert_eq!(spec.access, Access::User, "{} {}", m, p);
    }
}

#[tokio::test]
async fn anonymous_sessions_are_refused_and_writes_need_the_csrf_token() {
    let h = H::new(true);
    let anon = h.ctx.sessions.create(h.ctx.now());
    for (m, p) in ROUTES {
        let req = Request::builder()
            .method(*m)
            .uri(*p)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, format!("session={}", anon.id))
            .header("x-csrftoken", &anon.csrf_token)
            .body(Body::from("{}"))
            .unwrap();
        let mut req = req;
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
        crate::with_host(&mut req, &h.ctx);
        let resp = openalgo_desktop_lib::server::app(h.ctx.clone())
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{} {}", m, p);
    }
    // Signed in, but no CSRF token on a write: refused before the handler.
    for (m, p) in ROUTES.iter().filter(|(m, _)| *m != "GET") {
        let (s, v) = h.raw(m.parse().unwrap(), p, Some(nifty()), false).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{} {}", m, p);
        assert_eq!(
            v["message"],
            "Your session has expired. Refresh the page and try again."
        );
    }
    assert!(h.mock.calls().is_empty());
}

// --------------------------------------------------------- validation

#[tokio::test]
async fn chain_tools_validate_like_the_web() {
    let h = H::new(true);
    for p in [
        "/oitracker/api/oi-data",
        "/oitracker/api/maxpain",
        "/gex/api/gex-data",
        "/ivsmile/api/iv-smile-data",
        "/gammadensity/api/gamma-data",
        "/oiprofile/api/profile-data",
    ] {
        let cases = [
            (
                json!({"underlying": "NIFTY", "exchange": "NFO"}),
                "underlying, exchange, and expiry_date are required",
            ),
            (
                json!({"underlying": "nifty", "exchange": "NFO", "expiry_date": "30OCT26"}),
                "Invalid input format",
            ),
            (
                json!({"underlying": "NIFTY", "exchange": "N-FO", "expiry_date": "30OCT26"}),
                "Invalid input format",
            ),
            (
                json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_date": "30-OCT-26"}),
                "Invalid expiry_date format. Expected DDMMMYY",
            ),
            (
                json!({"underlying": 5, "exchange": "NFO", "expiry_date": "30OCT26"}),
                "underlying, exchange, and expiry_date are required",
            ),
        ];
        for (body, msg) in cases {
            let (s, v) = h.post(p, body).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{}", p);
            assert_eq!(v, json!({"status": "error", "message": msg}), "{}", p);
        }
    }
    let mut b = nifty();
    b["interval"] = json!("1h");
    let (s, v) = h.post("/oiprofile/api/profile-data", b).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Invalid interval. Allowed: 15m, 1m, 5m");
    assert!(h.mock.calls().is_empty());
}

#[tokio::test]
async fn history_tools_validate_like_the_web() {
    let h = H::new(true);
    let bad = |v: &Value, msg: &str| assert_eq!(v, &json!({"status": "error", "message": msg}));
    for p in [
        "/ivchart/api/iv-data",
        "/ivchart/api/default-symbols",
        "/straddle/api/straddle-data",
        "/straddlepnl/api/simulate",
    ] {
        let (s, v) = h.post(p, json!({"underlying": "NIFTY"})).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", p);
        bad(&v, "underlying, exchange, and expiry_date are required");
    }
    let mut b = nifty();
    b["adjustment_points"] = json!(0);
    let (s, v) = h.post("/straddlepnl/api/simulate", b).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    bad(&v, "adjustment_points must be >= 1");
    let mut b = nifty();
    b["lots"] = json!(0);
    bad(
        &h.post("/straddlepnl/api/simulate", b).await.1,
        "lot_size and lots must be >= 1",
    );
    let mut b = nifty();
    b["days"] = json!("three");
    let (s, v) = h.post("/straddle/api/straddle-data", b).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["message"].as_str().unwrap().contains("whole number"));

    let (s, v) = h
        .post(
            "/volsurface/api/surface-data",
            json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_dates": []}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    bad(&v, "expiry_dates must be a non-empty list");
    bad(
        &h.post("/volsurface/api/surface-data", json!({"exchange": "NFO"}))
            .await
            .1,
        "underlying and exchange are required",
    );

    for p in [
        "/strategybuilder/api/strategy-chart",
        "/strategybuilder/api/multi-strike-oi",
    ] {
        bad(
            &h.post(p, json!({"underlying": "NIFTY"})).await.1,
            "underlying and exchange are required",
        );
        bad(
            &h.post(
                p,
                json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "legs": []}),
            )
            .await
            .1,
            "At least one leg is required",
        );
        let (s, v) = h
            .post(p, json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "legs": [{"symbol": "X", "side": "BUY", "exchange": "NFO", "segment": "FUTURE"}]}))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        bad(&v, "No active option legs provided");
        let leg = json!([{"symbol": "NIFTY30OCT2625000CE", "side": "BUY", "exchange": "NFO"}]);
        for (start, end, msg) in [
            (
                "2026-02-30",
                "",
                "start_date must be a date in YYYY-MM-DD form",
            ),
            (
                "2026-10-01",
                "2026-09-01",
                "end_date cannot be earlier than start_date",
            ),
            (
                "1999-01-01",
                "2000-01-01",
                "start_date cannot be earlier than 2000-01-01",
            ),
            (
                "2024-01-01",
                "2026-01-01",
                "the window cannot be wider than 400 days",
            ),
        ] {
            let (s, v) = h
                .post(p, json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "legs": leg, "start_date": start, "end_date": end}))
                .await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{} {}", p, start);
            bad(&v, msg);
        }
    }
    let (s, v) = h.get("/straddlepnl/api/lotsize?underlying=NIFTY").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    bad(&v, "underlying and exchange required");
    let (s, v) = h.get("/arbitrage/api/universe?exchanges=NSE").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    bad(
        &v,
        "No supported exchanges in ['NSE']. Supported: NFO, MCX, BFO, CDS",
    );
    assert!(h.calls(|c| matches!(c, MockCall::History(_))) == 0);
}

/// Expiries that are not a real `DDMMMYY` day: multibyte (one split a
/// character in the strike lookup and panicked the request), short, long,
/// control characters, embedded or trailing text, impossible days.
const BAD_EXPIRIES: &[&str] = &[
    "1\u{e9}CT26",
    "\u{e9}0OCT26",
    "30OCT2\u{e9}",
    "30\u{d4}CT26",
    "\u{ff13}0OCT26",
    "30OC",
    "3",
    "30OCT2026",
    "30OCT26X",
    "30OCT26\n1",
    "30\u{7}CT26",
    "30 OCT26",
    "30-OC-26",
    "31FEB26",
    "29FEB27",
    "00OCT26",
    "32OCT26",
    "30XYZ26",
];

fn query_escape(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{:02X}", b)
            }
        })
        .collect()
}

/// TOOLS-06/T16: every option, history, scalping and public options route
/// refuses such an expiry with its documented answer before any master or
/// broker lookup, sends nothing to the broker, and keeps serving. MCP's
/// option tools post to the same `/api/v1` handlers.
#[tokio::test]
async fn malformed_expiries_are_refused_before_any_broker_request() {
    use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
    use openalgo_desktop_lib::services::options_service::Expiry;
    let h = H::new(true);
    let key = ApiKeyService::current(&h.ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    let bad_format =
        json!({"status": "error", "message": "Invalid expiry_date format. Expected DDMMMYY"});
    for x in BAD_EXPIRIES {
        let body = json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_date": x});
        for p in [
            "/oitracker/api/oi-data",
            "/oitracker/api/maxpain",
            "/gex/api/gex-data",
            "/ivsmile/api/iv-smile-data",
            "/gammadensity/api/gamma-data",
            "/oiprofile/api/profile-data",
            "/ivchart/api/iv-data",
            "/ivchart/api/default-symbols",
            "/straddle/api/straddle-data",
            "/straddlepnl/api/simulate",
        ] {
            let (s, v) = h.post(p, body.clone()).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{} {:?}", p, x);
            assert_eq!(v, bad_format, "{} {:?}", p, x);
        }
        let (s, v) = h
            .post(
                "/volsurface/api/surface-data",
                json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_dates": ["30OCT26", x]}),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "volsurface {:?}", x);
        assert_eq!(v, bad_format, "volsurface {:?}", x);
        // The scalping ladder accepts the web's spellings with `-` or a
        // space ("30 OCT26" is 30OCT26); anything else is refused.
        let spelled = x.replace(['-', ' '], "");
        if Expiry::parse(&spelled).is_none() {
            let (s, v) = h
                .get(&format!(
                    "/scalping/api/strikes?underlying=NIFTY&exchange=NFO&expiry={}",
                    query_escape(x)
                ))
                .await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "scalping {:?} {}", x, v);
        }
        // The public API answers as the web does for a date no contract
        // has (its strike lookup comes back empty), without the LTP fetch
        // the web makes first.
        let no_strikes = format!(
            "No strikes found for NIFTY expiring {}. Please check expiry date or update master contract.",
            x
        );
        for (p, mut b) in [
            (
                "/api/v1/optionchain",
                json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": x}),
            ),
            (
                "/api/v1/optionsymbol",
                json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": x, "offset": "ATM", "option_type": "CE"}),
            ),
            (
                "/api/v1/syntheticfuture",
                json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": x}),
            ),
        ] {
            b["apikey"] = json!(key);
            let (s, v) = h.post(p, b).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{} {:?} {}", p, x, v);
            assert_eq!(v["status"], "error", "{} {:?}", p, x);
            assert_eq!(v["message"], no_strikes.as_str(), "{} {:?}", p, x);
        }
    }
    assert!(
        h.mock.calls().is_empty(),
        "a refused expiry reached the broker: {:?}",
        h.mock.calls()
    );
    // Still serving, and a real leap day is a valid expiry (not refused
    // for its format).
    let (s, _) = h.post("/oitracker/api/oi-data", nifty()).await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = h
        .post(
            "/ivchart/api/default-symbols",
            json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_date": "29feb28"}),
        )
        .await;
    assert_ne!(v, bad_format);
}

#[tokio::test]
async fn tools_ask_for_a_broker_connection_first() {
    let h = H::new(false);
    for (p, body) in [
        ("/oitracker/api/oi-data", nifty()),
        ("/gex/api/gex-data", nifty()),
        ("/ivchart/api/iv-data", nifty()),
        ("/straddle/api/straddle-data", nifty()),
        (
            "/volsurface/api/surface-data",
            json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_dates": ["30OCT26"]}),
        ),
        ("/strikeprofile/api/profile-data", strike_profile_body()),
    ] {
        let (s, v) = h.post(p, body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", p);
        assert_eq!(
            v["message"], "Connect your broker to load this data.",
            "{}",
            p
        );
    }
    for p in ["/oiprofile/api/intervals", "/straddle/api/intervals"] {
        let (s, v) = h.get(p).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", p);
        assert_eq!(v["message"], "Connect your broker to load this data.");
    }
    // Master-contract reads work without a broker.
    assert_eq!(
        h.get("/straddlepnl/api/lotsize?underlying=nifty&exchange=nfo")
            .await
            .1,
        json!({"status": "success", "lotsize": 75})
    );
    assert_eq!(
        h.get("/straddlepnl/api/lotsize?underlying=BANKNIFTY&exchange=NFO")
            .await
            .1,
        json!({"status": "success", "lotsize": null})
    );
}

// ------------------------------------------------------------- shapes

#[tokio::test]
async fn oi_tracker_and_max_pain() {
    let h = H::new(true);
    let (s, v) = h.post("/oitracker/api/oi-data", nifty()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        keys(&v),
        vec![
            "atm_strike",
            "chain",
            "expiry_date",
            "futures_price",
            "lot_size",
            "pcr_oi",
            "pcr_volume",
            "spot_price",
            "status",
            "total_ce_oi",
            "total_pe_oi",
            "underlying"
        ]
    );
    assert_eq!(v["underlying"], "NIFTY");
    assert_eq!(v["spot_price"], json!(25010));
    assert_eq!(v["futures_price"], json!(25060));
    assert_eq!(v["atm_strike"], json!(25000.0));
    assert_eq!(v["lot_size"], json!(75));
    assert_eq!(v["total_ce_oi"], json!(15000));
    assert_eq!(v["total_pe_oi"], json!(15000));
    assert_eq!(v["pcr_oi"], json!(1.0));
    assert_eq!(
        v["chain"][0],
        json!({"strike": 24800.0, "ce_oi": 1000, "pe_oi": 5000})
    );
    // The chain is one batched quote call, not a call per option.
    assert_eq!(h.calls(|c| matches!(c, MockCall::MultiQuotes(_))), 1);
    assert_eq!(h.calls(|c| matches!(c, MockCall::Quote(_))), 2); // spot + future

    let (s, v) = h.post("/oitracker/api/maxpain", nifty()).await;
    assert_eq!(s, StatusCode::OK);
    // Worked by hand, not by the kernel under test (TOOLS-15): CE OI
    // 1000..5000 and PE OI 5000..1000 on 24800..25200. At 25000 CE writers
    // lose 200 x 1000 + 100 x 2000 and PE writers 100 x 2000 + 200 x 1000:
    // 800,000, the least of the five.
    assert_eq!(v["max_pain_strike"], json!(25000.0));
    let pain = |strike: f64, ce: f64, pe: f64, cr: f64| json!({"strike": strike, "ce_pain": ce, "pe_pain": pe, "total_pain": ce + pe, "total_pain_cr": cr});
    assert_eq!(
        v["pain_data"],
        json!([
            pain(24800.0, 0.0, 2_000_000.0, 0.2),
            pain(24900.0, 100_000.0, 1_000_000.0, 0.11),
            pain(25000.0, 400_000.0, 400_000.0, 0.08),
            pain(25100.0, 1_000_000.0, 100_000.0, 0.11),
            pain(25200.0, 2_000_000.0, 0.0, 0.2),
        ])
    );
    assert_eq!(
        keys(&v["pain_data"][0]),
        vec![
            "ce_pain",
            "pe_pain",
            "strike",
            "total_pain",
            "total_pain_cr"
        ]
    );
}

#[tokio::test]
async fn gex_smile_and_gamma_density() {
    let h = H::new(true);
    let (s, v) = h.post("/gex/api/gex-data", nifty()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        keys(&v),
        vec![
            "atm_strike",
            "chain",
            "expiry_date",
            "futures_price",
            "lot_size",
            "pcr_oi",
            "spot_price",
            "status",
            "total_ce_gex",
            "total_ce_oi",
            "total_net_gex",
            "total_pe_gex",
            "total_pe_oi",
            "underlying"
        ]
    );
    let row = &v["chain"][2];
    assert_eq!(
        keys(row),
        vec!["ce_gamma", "ce_gex", "ce_oi", "net_gex", "pe_gamma", "pe_gex", "pe_oi", "strike"]
    );
    // Worked outside the kernel under test (TOOLS-16): the 25000 call at
    // 130 against the spot 25010, 25.2292 days to 30 Oct 15:30 IST, r = 0,
    // inverts (bisection on the Black-76 price) to an IV of 4.7638% and a
    // gamma of 0.00127270, 0.001273 at six places; GEX = 0.001273 x 3000 OI
    // x 75 lot = 286.425.
    assert_eq!(row["strike"].as_f64(), Some(25_000.0));
    assert_eq!(row["ce_gamma"], json!(0.001273), "{}", row);
    assert!(
        (row["ce_gex"].as_f64().unwrap() - 286.425).abs() <= 0.005 + 1e-9,
        "{}",
        row
    );

    let (s, v) = h.post("/ivsmile/api/iv-smile-data", nifty()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        keys(&v),
        vec![
            "atm_iv",
            "atm_strike",
            "chain",
            "expiry_date",
            "skew",
            "spot_price",
            "status",
            "underlying"
        ]
    );
    assert!(v["chain"][2]["ce_iv"].as_f64().unwrap() > 0.0);
    assert!(v["atm_iv"].as_f64().unwrap() > 0.0);

    let (s, v) = h.post("/gammadensity/api/gamma-data", nifty()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        keys(&v),
        vec![
            "atm_iv",
            "atm_iv_source",
            "atm_strike",
            "chain",
            "dte_days",
            "estimated_iv_legs",
            "exchange",
            "expiry_band",
            "expiry_date",
            "forward_price",
            "interest_rate",
            "intraday_band",
            "one_sigma_high",
            "one_sigma_low",
            "peak_expiry_strike",
            "peak_intraday_strike",
            "pricing_reference",
            "sigma_move",
            "spot_price",
            "status",
            "two_sigma_high",
            "two_sigma_low",
            "underlying"
        ]
    );
    // Desktop provenance (TOOLS-15): every leg priced, against the forward.
    assert_eq!(v["atm_iv_source"], "atm");
    assert_eq!(v["estimated_iv_legs"], 0);
    assert_eq!(v["pricing_reference"], "forward");
    assert_eq!(
        keys(&v["chain"][0]),
        vec![
            "ce_oi",
            "density_expiry",
            "density_intraday",
            "iv",
            "pe_oi",
            "strike"
        ]
    );
    // Synthetic future: 25000 + 130 - 110.
    assert_eq!(v["forward_price"], json!(25020.0));
    assert_eq!(v["sigma_move"], v["intraday_band"]["sigma_move"]);
    assert!(v["dte_days"].as_f64().unwrap() > 25.0);
}

/// TOOLS-15: with no option price to invert, Gamma Density says its IV is
/// the assumed 15% and how many legs that covers, instead of presenting the
/// bands as market-implied. Band worked by hand: 25010 x 0.15 x
/// sqrt(1/365) = 196.3625.
#[tokio::test]
async fn gamma_density_says_when_its_iv_is_assumed() {
    let h = H::new(true);
    for (i, k) in STRIKES.iter().enumerate() {
        for (t, oi) in [("CE", ce_oi(i)), ("PE", pe_oi(i))] {
            h.mock.set_quote(quote(
                &format!("NIFTY30OCT26{}{}", *k as i64, t),
                "NFO",
                0.0,
                oi,
            ));
        }
    }
    let (s, v) = h.post("/gammadensity/api/gamma-data", nifty()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["atm_iv_source"], "assumed");
    assert_eq!(v["atm_iv"], json!(15.0));
    assert_eq!(v["estimated_iv_legs"], 10);
    assert_eq!(v["sigma_move"], json!(196.36));
    assert_eq!(v["one_sigma_low"], json!(24813.64));
    assert_eq!(v["one_sigma_high"], json!(25206.36));
    assert!(v["chain"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["iv"].is_null()));
}

#[tokio::test]
async fn oi_profile_with_previous_day_oi() {
    let h = H::new(true);
    let mut b = nifty();
    b["interval"] = json!("1m");
    b["days"] = json!(1);
    let (s, v) = h.post("/oiprofile/api/profile-data", b).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        keys(&v),
        vec![
            "atm_strike",
            "candles",
            "expiry_date",
            "futures_symbol",
            "interval",
            "lot_size",
            "oi_chain",
            "spot_price",
            "status",
            "underlying"
        ]
    );
    assert_eq!(v["futures_symbol"], "NIFTY30OCT26FUT");
    // Capped to the last trading date: Monday's three candles, with `time`.
    let c = v["candles"].as_array().unwrap();
    assert_eq!(c.len(), 3);
    assert_eq!(c[0]["time"], c[0]["timestamp"]);
    // Previous OI is the second-last daily candle's (500).
    assert_eq!(
        v["oi_chain"][0],
        json!({"strike": 24800.0, "ce_oi": 1000, "pe_oi": 5000, "ce_oi_change": 500.0, "pe_oi_change": 4500.0})
    );
    // 1 futures history + one daily history per contract with OI.
    assert_eq!(h.calls(|c| matches!(c, MockCall::History(_))), 11);
}

/// TOOLS-04, TOOLS-13: an OI figure is not worked out over a leg whose
/// open interest did not come back (here a leg the broker answered
/// without a quote): it would total as zero and read as a quiet strike.
/// The price-only IV Smile still answers.
#[tokio::test]
async fn oi_figures_are_refused_when_a_legs_oi_did_not_come_back() {
    let h = H::new(true);
    h.mock.quotes.lock().remove("NFO:NIFTY30OCT2625100PE");
    let want = "Open interest did not come back from your broker for 1 of 10 options of NIFTY 30OCT26, so the OI figures are not shown: a missing value would read as zero. Try again shortly.";
    for p in [
        "/oitracker/api/oi-data",
        "/oitracker/api/maxpain",
        "/gex/api/gex-data",
        "/gammadensity/api/gamma-data",
    ] {
        let (s, v) = h.post(p, nifty()).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{} {}", p, v);
        assert_eq!(v["message"], want, "{}", p);
    }
    let mut b = nifty();
    b["interval"] = json!("1m");
    let (s, v) = h.post("/oiprofile/api/profile-data", b).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{}", v);
    let (s, v) = h.post("/ivsmile/api/iv-smile-data", nifty()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["chain"][3]["pe_iv"], Value::Null);
}

/// TOOLS-13: with zero open interest at every strike there is no max pain;
/// the lowest strike used to win the tie and read as a finding.
#[tokio::test]
async fn max_pain_is_not_shown_over_all_zero_open_interest() {
    let h = H::new(true);
    {
        let mut q = h.mock.quotes.lock();
        for (k, v) in q.iter_mut() {
            if k.starts_with("NFO:NIFTY30OCT26") && !k.ends_with("FUT") {
                v.oi = 0;
            }
        }
    }
    let (s, v) = h.post("/oitracker/api/maxpain", nifty()).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{}", v);
    assert_eq!(
        v["message"],
        "Open interest is zero at every strike of this expiry, so there is no max pain to show yet."
    );
}

/// TOOLS-14: when the previous day's OI cannot be had (the history fails,
/// or holds no previous day), the change is unknown (null) and counted,
/// never the whole current OI as one day's build-up.
#[tokio::test]
async fn oi_profile_shows_no_change_where_the_previous_day_is_unknown() {
    let h = H::new(true);
    *h.mock.history.lock() = Some(Err("history unavailable".into()));
    let mut b = nifty();
    b["interval"] = json!("1m");
    b["days"] = json!(1);
    let (s, v) = h.post("/oiprofile/api/profile-data", b.clone()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["oi_chain"][0],
        json!({"strike": 24800.0, "ce_oi": 1000, "pe_oi": 5000, "ce_oi_change": null, "pe_oi_change": null})
    );
    assert_eq!(v["oi_change_loaded"], 0);
    assert_eq!(v["oi_change_requested"], 10);
    assert_eq!(v["oi_change_failed"], 10);
    assert_eq!(
        v["message"],
        "The daily OI change is shown for 0 of 10 option contracts: the broker's history did not give the previous day for 10. The others show no change rather than a guess. Refresh in a minute to try again."
    );

    // One daily candle: no previous day in it, so no change either; a
    // real previous OI of zero, in a history whose bars carry OI, is a
    // reading (the whole OI is new).
    let h = H::new(true);
    let one = Candle {
        timestamp: ist(5, 9, 15).timestamp(),
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1,
        oi: 0,
    };
    *h.mock.history.lock() = Some(Ok(vec![one]));
    let (_, v) = h.post("/oiprofile/api/profile-data", b.clone()).await;
    assert_eq!(v["oi_chain"][0]["ce_oi_change"], Value::Null);
    let zero_before = Candle {
        timestamp: ist(2, 15, 29).timestamp(),
        ..one
    };
    let today = Candle { oi: 1000, ..one };
    *h.mock.history.lock() = Some(Ok(vec![zero_before, today]));
    let (_, v) = h.post("/oiprofile/api/profile-data", b).await;
    assert_eq!(v["oi_chain"][0]["ce_oi_change"], json!(1000.0));
    assert!(v.get("oi_change_failed").is_none());
}

/// TOOLS-14 (review finding, pocketful-data): a daily history whose bars
/// all carry OI 0 has no open interest in it. Pocketful's chart candles
/// are such a history (for BFO and MCX options, and whenever its NFO
/// endpoint falls back), so the previous day's OI is not read from it:
/// the change is null and counted, never the whole OI as one day's
/// build-up.
#[tokio::test]
async fn oi_profile_reads_no_previous_day_from_a_history_without_oi() {
    let h = H::new(true);
    let daily = |d: u32| Candle {
        timestamp: ist(d, 0, 0).timestamp(),
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1,
        oi: 0,
    };
    *h.mock.history.lock() = Some(Ok(vec![daily(1), daily(2), daily(5)]));
    let mut b = nifty();
    b["interval"] = json!("1m");
    b["days"] = json!(1);
    let (s, v) = h.post("/oiprofile/api/profile-data", b).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["oi_chain"][0],
        json!({"strike": 24800.0, "ce_oi": 1000, "pe_oi": 5000, "ce_oi_change": null, "pe_oi_change": null})
    );
    assert_eq!(v["oi_change_loaded"], 0);
    assert_eq!(v["oi_change_requested"], 10);
    assert_eq!(v["oi_change_failed"], 10);
    assert_eq!(
        v["message"],
        "The daily OI change is shown for 0 of 10 option contracts: the broker's history did not give the previous day for 10. The others show no change rather than a guess. Refresh in a minute to try again."
    );
}

#[tokio::test]
async fn iv_chart_and_default_symbols() {
    let h = H::new(true);
    let (s, v) = h.post("/ivchart/api/default-symbols", nifty()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v,
        json!({"status": "success", "data": {"ce_symbol": "NIFTY30OCT2625000CE", "pe_symbol": "NIFTY30OCT2625000PE", "atm_strike": 25000.0, "exchange": "NFO", "underlying_ltp": 25010}})
    );
    let mut b = nifty();
    b["interval"] = json!("1m");
    let (s, v) = h.post("/ivchart/api/iv-data", b).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let d = &v["data"];
    assert_eq!(
        keys(d),
        vec![
            "atm_strike",
            "ce_symbol",
            "interval",
            "pe_symbol",
            "series",
            "underlying",
            "underlying_ltp"
        ]
    );
    let series = d["series"].as_array().unwrap();
    assert_eq!(series.len(), 2);
    assert_eq!(series[0]["option_type"], "CE");
    let pts = series[0]["iv_data"].as_array().unwrap();
    assert_eq!(pts.len(), 3); // last trading date only
    assert_eq!(
        keys(&pts[0]),
        vec![
            "delta",
            "gamma",
            "iv",
            "option_price",
            "theta",
            "time",
            "underlying_price",
            "vega"
        ]
    );
    assert_eq!(h.calls(|c| matches!(c, MockCall::History(_))), 3);
}

#[tokio::test]
async fn straddle_chart_and_simulation() {
    let h = H::new(true);
    let (s, v) = h.post("/straddle/api/straddle-data", nifty()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let d = &v["data"];
    assert_eq!(
        keys(d),
        vec![
            "days_to_expiry",
            "expiry_date",
            "interval",
            "series",
            "underlying",
            "underlying_ltp"
        ]
    );
    assert_eq!(d["days_to_expiry"], json!(25));
    assert_eq!(
        keys(&d["series"][0]),
        vec![
            "atm_strike",
            "ce_price",
            "pe_price",
            "spot",
            "straddle",
            "synthetic_future",
            "time"
        ]
    );

    let mut b = nifty();
    b["lot_size"] = json!(75);
    b["lots"] = json!(2);
    b["adjustment_points"] = json!(100);
    let (s, v) = h.post("/straddlepnl/api/simulate", b).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let d = &v["data"];
    assert_eq!(
        keys(d),
        vec![
            "adjustment_points",
            "days_to_expiry",
            "expiry_date",
            "interval",
            "lot_size",
            "lots",
            "pnl_series",
            "quantity",
            "summary",
            "trades",
            "underlying",
            "underlying_ltp"
        ]
    );
    assert_eq!(d["quantity"], json!(150));
    assert_eq!(d["trades"][0]["type"], "ENTRY");
    assert_eq!(
        d["trades"].as_array().unwrap().last().unwrap()["type"],
        "EXIT"
    );
    assert_eq!(
        keys(&d["summary"]),
        vec!["max_pnl", "min_pnl", "total_adjustments", "total_pnl"]
    );
}

#[tokio::test]
async fn vol_surface_uses_one_quote_batch_per_expiry() {
    let h = H::new(true);
    let (s, v) = h
        .post(
            "/volsurface/api/surface-data",
            json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_dates": ["30OCT26", "27NOV26"], "strike_count": 2}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let d = &v["data"];
    assert_eq!(
        keys(d),
        vec![
            "atm_strike",
            "expiries",
            "strikes",
            "surface",
            "underlying",
            "underlying_ltp"
        ]
    );
    assert_eq!(d["strikes"], json!(STRIKES));
    assert_eq!(d["surface"].as_array().unwrap().len(), 2);
    assert_eq!(d["surface"][0].as_array().unwrap().len(), 5);
    assert!(d["surface"][0][2].as_f64().unwrap() > 0.0);
    assert_eq!(keys(&d["expiries"][0]), vec!["date", "dte"]);
    assert_eq!(h.calls(|c| matches!(c, MockCall::MultiQuotes(_))), 2);
}

#[tokio::test]
async fn strategy_builder_charts_dedupe_legs() {
    let h = H::new(true);
    let legs = json!([
        {"symbol": "NIFTY30OCT2625000CE", "exchange": "NFO", "side": "SELL", "price": 130, "strike": 25000, "optionType": "CE", "expiry": "30OCT26"},
        {"symbol": "NIFTY30OCT2625000PE", "exchange": "NFO", "side": "SELL", "price": 110},
        {"symbol": "NIFTY30OCT2625000CE", "exchange": "NFO", "side": "BUY", "price": 100},
        {"symbol": "NIFTY30OCT26FUT", "exchange": "NFO", "side": "BUY", "segment": "FUTURE"}
    ]);
    let body = json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "interval": "1m", "days": 1, "legs": legs});
    let (s, v) = h
        .post("/strategybuilder/api/strategy-chart", body.clone())
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let d = &v["data"];
    assert_eq!(
        keys(d),
        vec![
            "entry_abs_premium",
            "entry_net_premium",
            "interval",
            "legs_used",
            "series",
            "tag",
            "underlying",
            "underlying_available",
            "underlying_ltp"
        ]
    );
    assert_eq!(d["legs_used"], json!(3));
    assert_eq!(d["tag"], "credit"); // 130 + 110 - 100
    assert_eq!(d["entry_net_premium"], json!(140.0));
    assert_eq!(d["underlying_available"], json!(true));
    assert_eq!(d["series"].as_array().unwrap().len(), 3);
    // Underlying plus two unique option legs.
    assert_eq!(h.calls(|c| matches!(c, MockCall::History(_))), 3);

    let (s, v) = h.post("/strategybuilder/api/multi-strike-oi", body).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let d = &v["data"];
    assert_eq!(
        keys(d),
        vec![
            "interval",
            "legs",
            "underlying",
            "underlying_available",
            "underlying_ltp",
            "underlying_series"
        ]
    );
    assert_eq!(d["legs"].as_array().unwrap().len(), 3);
    assert_eq!(d["legs"][0]["option_type"], "CE");
    assert_eq!(d["legs"][0]["has_oi"], json!(true));
    assert_eq!(d["legs"][0]["series"][0]["value"], json!(500.0));
}

#[tokio::test]
async fn intervals_variants() {
    let h = H::new(true);
    assert_eq!(
        h.get("/oiprofile/api/intervals").await.1,
        json!({"status": "success", "data": {"intervals": ["1m", "5m"]}})
    );
    assert_eq!(
        h.get("/ivchart/api/intervals").await.1,
        json!({"status": "success", "data": {"seconds": [], "minutes": ["1m", "5m"], "hours": []}})
    );
    let full = h.get("/straddle/api/intervals").await.1;
    assert_eq!(full["data"]["days"], json!(["D"]));
    assert_eq!(h.get("/straddlepnl/api/intervals").await.1, full);
    assert_eq!(h.get("/strategybuilder/api/intervals").await.1, full);
}

#[tokio::test]
async fn arbitrage_universe() {
    let h = H::new(false);
    let (s, v) = h.get("/arbitrage/api/universe").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v["data"]["counts"],
        json!({"underlyings": 2, "pairs": 2, "symbols": 4})
    );
    assert_eq!(v["data"]["pairs"][0]["id"], "NFO:NIFTY:near-next");
    let (_, v) = h.get("/arbitrage/api/universe?exchanges=mcx").await;
    assert_eq!(v["data"]["pairs"][0]["far"]["symbol"], "GOLD05FEB27FUT");
}

#[tokio::test]
async fn strategy_portfolio_crud() {
    let h = H::new(false);
    let p = "/api/strategy-portfolio";
    let body = json!({"name": "  Short straddle ", "watchlist": "mytrades", "underlying": "NIFTY", "exchange": "NFO", "expiry": "30OCT26", "legs": [{"symbol": "NIFTY30OCT2625000CE", "side": "SELL"}]});
    for (field, msg) in [
        ("name", "'name' is required"),
        ("watchlist", "'watchlist' is required"),
        ("underlying", "'underlying' is required"),
        ("exchange", "'exchange' is required"),
    ] {
        let mut b = body.clone();
        b[field] = json!("");
        let (s, v) = h.post(p, b).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(v, json!({"status": "error", "message": msg}));
    }
    let mut b = body.clone();
    b["watchlist"] = json!("live");
    assert_eq!(
        h.post(p, b).await.1["message"],
        "watchlist must be one of ['mytrades', 'simulation']"
    );
    let mut b = body.clone();
    b["legs"] = json!([]);
    assert_eq!(
        h.post(p, b).await.1["message"],
        "at least one leg is required"
    );
    let mut b = body.clone();
    b["name"] = json!("x".repeat(121));
    assert_eq!(
        h.post(p, b).await.1["message"],
        "name too long (max 120 chars)"
    );

    let (s, v) = h.post(p, body.clone()).await;
    assert_eq!(s, StatusCode::OK);
    let item = &v["item"];
    assert_eq!(
        keys(item),
        vec![
            "created_at",
            "exchange",
            "expiry",
            "id",
            "legs",
            "name",
            "notes",
            "underlying",
            "updated_at",
            "watchlist"
        ]
    );
    assert_eq!(item["name"], "Short straddle");
    assert_eq!(item["created_at"], "2026-10-05T04:30:00");
    let id = item["id"].as_i64().unwrap();

    let (s, v) = h.get(&format!("{}/{}", p, id)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["item"]["legs"][0]["side"], "SELL");
    assert_eq!(
        h.get(&format!("{}?watchlist=simulation", p)).await.1,
        json!({"status": "success", "items": []})
    );
    assert_eq!(
        h.get(&format!("{}?watchlist=other", p)).await.1,
        json!({"status": "error", "message": "invalid watchlist"})
    );
    assert_eq!(h.get(p).await.1["items"].as_array().unwrap().len(), 1);

    let mut b = body.clone();
    b["watchlist"] = json!("simulation");
    b["notes"] = json!("hedged");
    let (s, v) = h
        .raw(Method::PUT, &format!("{}/{}", p, id), Some(b.clone()), true)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["item"]["watchlist"], "simulation");
    assert_eq!(v["item"]["notes"], "hedged");
    let (s, v) = h
        .raw(Method::PUT, &format!("{}/999", p), Some(b), true)
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"status": "error", "message": "not found"})
        )
    );

    let (s, v) = h
        .raw(Method::DELETE, &format!("{}/{}", p, id), None, true)
        .await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "success"})));
    let (s, _) = h
        .raw(Method::DELETE, &format!("{}/{}", p, id), None, true)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = h.get(&format!("{}/{}", p, id)).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"status": "error", "message": "not found"})
        )
    );
    let (s, _) = h.get(&format!("{}/abc", p)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

// ------------------------------------------------------------ fan-out

/// In-flight counter for a fan-out probe.
#[derive(Default)]
struct Probe {
    now: AtomicUsize,
    max: AtomicUsize,
}

impl Probe {
    async fn work(&self, ms: u64) {
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(n, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        self.now.fetch_sub(1, Ordering::SeqCst);
    }
}

// ------------------------------------------------------- strike profile

fn strike_profile_body() -> Value {
    json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_dates": ["30OCT26", "27NOV26"]})
}

/// Web `strike_profile.py`: each refusal, in the blueprint's order, before
/// any broker request.
#[tokio::test]
async fn strike_profile_validates_like_the_web() {
    let h = H::new(true);
    let with = |k: &str, v: Value| {
        let mut b = strike_profile_body();
        b[k] = v;
        b
    };
    let cases = [
        (
            with("underlying", json!("")),
            "underlying and exchange are required",
        ),
        (
            with("exchange", json!(" ")),
            "underlying and exchange are required",
        ),
        (with("underlying", json!("nifty")), "Invalid input format"),
        // Python's str(None) is "None", which is not upper case.
        (with("underlying", Value::Null), "Invalid input format"),
        (with("exchange", json!("N-FO")), "Invalid input format"),
        (
            with("expiry_dates", json!([])),
            "Select at least one expiry.",
        ),
        (
            with("expiry_dates", json!("30OCT26")),
            "Select at least one expiry.",
        ),
        (
            with("expiry_dates", json!(["30OCT26", "30-OCT-26"])),
            "Invalid expiry format. Expected DDMMMYY",
        ),
        (with("range_pct", json!(3)), "Invalid strike range"),
        (with("range_pct", json!("wide")), "Invalid strike range"),
    ];
    for (body, msg) in cases {
        let (s, v) = h
            .post("/strikeprofile/api/profile-data", body.clone())
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", body);
        assert_eq!(v, json!({"status": "error", "message": msg}), "{}", body);
    }
    assert!(h.mock.calls().is_empty());
}

/// The Strike Profile over the mock chain: one option chain per expiry,
/// the web's shape, and figures worked outside the kernels under test (a
/// separate Black-76 in Python with `math.erf` and its own bisection).
///
/// Both expiries price the same: the 25000 call at 130 and put at 110 give
/// the forward 25020; the call inverts to 4.56% on 30 Oct (25.229 days)
/// and 3.14% on 27 Nov, with the same total variance, so gamma 0.001325
/// and delta 0.529 on both. Call GEX = 0.001325 x 3000 OI x 25010^2 x 1% /
/// 1e7 = 2.4864 crore, the put's the same negative; call DEX = 0.529 x
/// 3000 x 25010 / 1e7 = 3.9691, put DEX -0.471 x ... = -3.5339. Summed
/// over both expiries the call wall is 25100 and the put wall 25000; total
/// GEX, re-priced over 241 prices from 24800 to 25200, turns positive at
/// 24964.12.
#[tokio::test]
async fn strike_profile_by_strike_and_expiry() {
    let h = H::new(true);
    let (s, v) = h
        .post("/strikeprofile/api/profile-data", strike_profile_body())
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        keys(&v),
        vec![
            "as_of",
            "call_wall",
            "exchange",
            "expiries",
            "flip",
            "put_wall",
            "range_pct",
            "spot_price",
            "status",
            "strikes",
            "underlying"
        ]
    );
    assert_eq!(v["underlying"], "NIFTY");
    assert_eq!(v["exchange"], "NFO");
    assert_eq!(v["spot_price"].to_string(), "25010.0");
    assert_eq!(v["range_pct"].to_string(), "5.0");
    assert_eq!(v["as_of"], json!(ist(5, 10, 0).timestamp()));
    assert_eq!(
        v["strikes"],
        json!([24_800.0, 24_900.0, 25_000.0, 25_100.0, 25_200.0])
    );
    let near = &v["expiries"][0];
    assert_eq!(near["expiry"], "30OCT26");
    assert_eq!(near["expiry_ts"], json!(1_793_354_400));
    assert_eq!(near["dte"], json!(25));
    assert_eq!(near["years"], json!(0.069121));
    assert_eq!(near["forward"].to_string(), "25020.0");
    assert_eq!(near["lot_size"], json!(75));
    assert_eq!(near["available"], json!(true));
    assert_eq!(near["net_gex"], json!(0.2039));
    assert_eq!(near["net_dex"], json!(1.5591));
    assert_eq!(v["expiries"][1]["dte"], json!(53));
    assert_eq!(v["expiries"][1]["years"], json!(0.145833));
    assert_eq!(
        near["cells"][2],
        json!({
            "ce_oi": 3000, "pe_oi": 3000,
            "ce_gex": 2.4864, "pe_gex": -2.4864,
            "ce_dex": 3.9691, "pe_dex": -3.5339,
            "gex": 0.0, "dex": 0.4352,
            "unpriced": false,
        })
    );
    assert_eq!(v["call_wall"], json!(25_100.0));
    assert_eq!(v["put_wall"], json!(25_000.0));
    assert_eq!(v["flip"], json!(24_964.12));
    // One option chain (one batched quote) per expiry; 5% of spot fits in
    // the first chain's 20 strikes a side.
    assert_eq!(h.calls(|c| matches!(c, MockCall::MultiQuotes(_))), 2);

    // 10% needs 27 strikes a side over a 100 step: the first chain is
    // fetched again with the wider count.
    let mut b = strike_profile_body();
    b["range_pct"] = json!(10);
    let (s, v) = h.post("/strikeprofile/api/profile-data", b).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(h.calls(|c| matches!(c, MockCall::MultiQuotes(_))), 5);
    assert_eq!(v["range_pct"].to_string(), "10.0");

    // A later expiry with no listed strikes is marked unavailable; the
    // grid stands. Lower case is upper-cased, as on the web.
    let b = json!({"underlying": "NIFTY", "exchange": "NFO", "expiry_dates": ["30oct26", "24DEC26"], "range_pct": null});
    let (s, v) = h.post("/strikeprofile/api/profile-data", b).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["range_pct"], Value::Null);
    assert_eq!(v["expiries"][0]["expiry"], "30OCT26");
    assert_eq!(v["expiries"][1]["available"], json!(false));
    assert_eq!(
        v["expiries"][1]["message"],
        "No option data for this expiry."
    );
    assert_eq!(
        v["expiries"][1]["cells"],
        json!([null, null, null, null, null])
    );
}

/// Web parity (maintainer 2026-10-20: Strike Profile matches the web
/// exactly): the 25100 put's quote does not come back on either expiry, the
/// chain answers that leg with zeros, and the profile totals it as zero
/// open interest instead of refusing (the GEX dashboard refuses, TOOLS-13)
/// or marking the expiry unavailable. Worked outside the kernels like
/// `strike_profile_by_strike_and_expiry`: the 25100 call alone, IV 5.96%,
/// gamma 0.000998 and delta 0.4224, gives GEX 2.4970 and DEX 4.2257 crore;
/// without that put's -1.2485 and -2.8892, each expiry nets 1.4524 GEX and
/// 4.4483 DEX. The put wall stays at 25000.
#[tokio::test]
async fn strike_profile_totals_an_open_interest_gap_as_zero() {
    let h = H::new(true);
    h.mock.quotes.lock().remove("NFO:NIFTY30OCT2625100PE");
    h.mock.quotes.lock().remove("NFO:NIFTY27NOV2625100PE");
    let (s, v) = h
        .post("/strikeprofile/api/profile-data", strike_profile_body())
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    for e in v["expiries"].as_array().unwrap() {
        assert_eq!(e["available"], json!(true), "{}", e["expiry"]);
        assert_eq!(
            e["cells"][3],
            json!({
                "ce_oi": 4000, "pe_oi": 0,
                "ce_gex": 2.497, "pe_gex": 0.0,
                "ce_dex": 4.2257, "pe_dex": 0.0,
                "gex": 2.497, "dex": 4.2257,
                "unpriced": false,
            }),
            "{}",
            e["expiry"]
        );
        assert_eq!(e["net_gex"], json!(1.4524), "{}", e["expiry"]);
        assert_eq!(e["net_dex"], json!(4.4483), "{}", e["expiry"]);
    }
    assert_eq!(v["call_wall"], json!(25_100.0));
    assert_eq!(v["put_wall"], json!(25_000.0));
}

#[tokio::test]
async fn fan_out_is_bounded_and_keeps_order() {
    let gate = Gate::new(3, Duration::ZERO);
    let probe = Probe::default();
    let out = fan_out(&gate, (0..20).collect(), |i: u64| {
        let probe = &probe;
        async move {
            probe.work(5 + (i % 4) * 3).await;
            i * 10
        }
    })
    .await;
    assert_eq!(out, (0..20).map(|i| i * 10).collect::<Vec<_>>());
    assert_eq!(probe.max.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn concurrent_fan_outs_share_the_gate() {
    let gate = Gate::new(2, Duration::ZERO);
    let probe = Probe::default();
    let run = || {
        fan_out(&gate, (0..10).collect(), |_: u32| async {
            probe.work(5).await
        })
    };
    tokio::join!(run(), run(), run());
    assert_eq!(probe.max.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn fan_out_spaces_its_starts() {
    let gate = Gate::new(4, Duration::from_millis(100));
    let t0 = tokio::time::Instant::now();
    let starts = fan_out(
        &gate,
        (0..5).collect(),
        |_: u32| async move { t0.elapsed() },
    )
    .await;
    for w in starts.windows(2) {
        assert!(w[1] - w[0] >= Duration::from_millis(100), "{:?}", starts);
    }
}

#[test]
fn the_shared_history_gate_is_bounded() {
    use openalgo_desktop_lib::services::tools_service::{history_gate, HISTORY_CONCURRENCY};
    assert_eq!(history_gate().limit(), HISTORY_CONCURRENCY);
    const { assert!(HISTORY_CONCURRENCY <= 3) };
}
