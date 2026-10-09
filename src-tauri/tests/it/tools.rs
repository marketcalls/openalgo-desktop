//! Options and portfolio tools: every route the tool pages call (OI
//! tracker, max pain, OI profile, IV chart, gamma density, straddles,
//! vol surface, GEX, IV smile, arbitrage, Strategy Builder charts and the
//! strategy portfolio), driven in-process against the full router with the
//! mock broker connected; plus the bounded history fan-out.
//!
//! Self-contained (no shared support module) so it can be folded into the
//! consolidated integration crate unchanged.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use openalgo_desktop_lib::analytics::chain::{max_pain, StrikeOi};
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
    let chain: Vec<StrikeOi> = STRIKES
        .iter()
        .enumerate()
        .map(|(i, k)| StrikeOi {
            strike: *k,
            ce_oi: ce_oi(i) as f64,
            pe_oi: pe_oi(i) as f64,
        })
        .collect();
    let (want, pain) = max_pain(&chain).unwrap();
    assert_eq!(v["max_pain_strike"], json!(want));
    assert_eq!(v["pain_data"], serde_json::to_value(&pain).unwrap());
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
    let g = row["ce_gamma"].as_f64().unwrap();
    assert!(g > 0.0);
    let want = (g * 3000.0 * 75.0 * 100.0).round() / 100.0;
    assert!((row["ce_gex"].as_f64().unwrap() - want).abs() < 0.011);

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
            "atm_strike",
            "chain",
            "dte_days",
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
            "sigma_move",
            "spot_price",
            "status",
            "two_sigma_high",
            "two_sigma_low",
            "underlying"
        ]
    );
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
