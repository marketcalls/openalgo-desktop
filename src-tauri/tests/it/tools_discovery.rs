//! Expiry discovery for the option tools, end to end (TOOLS-01, TOOLS-03;
//! gates T01, T02 and T04): a broker's raw master file, through its native
//! parser and the app's master snapshot, to the session route every tool
//! page calls (`GET /search/api/expiries?...&instrumenttype=options`).
//!
//! On 12 October 2026 a trader on Fyers saw an empty Select Expiry, a
//! disabled Refresh and an endless spinner on the Option Chain: Fyers
//! stores the contract description in `name` (`NIFTY 13 Oct 26 25000
//! CE`), and discovery compared `name` with the underlying. The Fyers
//! files under `fixtures/brokers/fyers/live/` carry contracts after that
//! day; every clock here is fixed. The session route's answer for them is
//! the golden file `tests/fixtures/tools/fyers_nifty_option_expiries.json`,
//! which the Option Chain page test mounts the real page on.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::Router;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use openalgo_desktop_lib::brokers::common::master_contract::parse_oa_expiry;
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolGeneration, SymbolResolver};
use openalgo_desktop_lib::brokers::fyers::{FyersBroker, FyersUrls};
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::services::auth_service::AuthService;
use openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService;
use openalgo_desktop_lib::services::options_service::listed_live;
use openalgo_desktop_lib::services::search_ui_service::expiries;
use openalgo_desktop_lib::services::symbol_service::expiry_dates;
use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const USER: &str = "trader";

macro_rules! fx {
    ($path:literal) => {
        include_str!(concat!("../fixtures/brokers/", $path))
    };
}

/// The session route's answer for NIFTY options on the live Fyers master,
/// shared with the Option Chain page test.
const FYERS_NIFTY_EXPIRIES: &str =
    include_str!("../../../tests/fixtures/tools/fyers_nifty_option_expiries.json");
/// The session route's answer while no master is loaded, shared with the
/// tool page tests.
const MASTER_NOT_READY: &str =
    include_str!("../../../tests/fixtures/tools/expiries_master_not_ready.json");

fn ist(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(y, mo, d, h, mi, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

fn golden(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

// ------------------------------------------------------------ fake Fyers

/// Fyers' public master files, with the live derivative files.
const FYERS_FILES: &[(&str, &str)] = &[
    ("NSE_CM.csv", fx!("fyers/NSE_CM.csv")),
    ("BSE_CM.csv", fx!("fyers/BSE_CM.csv")),
    ("NSE_FO.csv", fx!("fyers/live/NSE_FO.csv")),
    ("BSE_FO.csv", fx!("fyers/live/BSE_FO.csv")),
    (
        "NSE_CD_sym_master.json",
        fx!("fyers/NSE_CD_sym_master.json"),
    ),
    (
        "MCX_COM_sym_master.json",
        fx!("fyers/MCX_COM_sym_master.json"),
    ),
    (
        "index_hsm_mapping.json",
        fx!("fyers/index_hsm_mapping.json"),
    ),
];

/// A local host serving `FYERS_FILES` under `/sym_details/` (ephemeral
/// port); anything else is a 404, so no request leaves the machine.
async fn fyers_host() -> String {
    let app = Router::new().fallback(|uri: Uri| async move {
        let path = uri.path().to_string();
        match FYERS_FILES
            .iter()
            .find(|(name, _)| path == format!("/sym_details/{}", name))
        {
            Some((_, body)) => (StatusCode::OK, *body).into_response(),
            None => (StatusCode::NOT_FOUND, "{}").into_response(),
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

/// The Fyers master as its native parser reads the live files.
fn fyers_rows() -> Vec<SymToken> {
    use openalgo_desktop_lib::brokers::fyers::master_contract::parse_all;
    let files: HashMap<&str, String> = [
        ("NSE_CM", "NSE_CM.csv"),
        ("BSE_CM", "BSE_CM.csv"),
        ("NSE_FO", "NSE_FO.csv"),
        ("BSE_FO", "BSE_FO.csv"),
        ("NSE_CD", "NSE_CD_sym_master.json"),
        ("MCX_COM", "MCX_COM_sym_master.json"),
    ]
    .into_iter()
    .map(|(k, f)| {
        let body = FYERS_FILES.iter().find(|(n, _)| *n == f).unwrap().1;
        (k, body.to_string())
    })
    .collect();
    let names: HashMap<String, String> =
        serde_json::from_str(fx!("fyers/index_hsm_mapping.json")).unwrap();
    parse_all(&files, &names).unwrap()
}

// --------------------------------------------------------------- the app

struct App {
    ctx: Arc<AppState>,
    clock: Arc<ManualClock>,
    cookie: String,
    csrf: String,
    _dir: tempfile::TempDir,
}

impl App {
    /// Signed in, Fyers registered against the local file host, nothing
    /// downloaded yet.
    async fn fyers(now: DateTime<Utc>) -> Self {
        let base = fyers_host().await;
        let ws = format!("{}/feed", base.replacen("http", "ws", 1));
        let dir = tempfile::tempdir().unwrap();
        let symbols = SymbolResolver::new();
        let fyers = Arc::new(FyersBroker::with_urls(
            symbols.clone(),
            FyersUrls {
                api: base.clone(),
                public: format!("{}/sym_details", base),
                hsm: ws.clone(),
                tbt: ws.clone(),
                order_ws: ws,
            },
        ));
        let clock = ManualClock::new(now);
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: clock.clone(),
                brokers: Arc::new(BrokerRegistry::with_symbols(
                    symbols,
                    vec![fyers as Arc<dyn Broker>],
                )),
            },
        )
        .unwrap();
        AuthService::setup(&ctx, USER, "trader@example.com", "Secret@123").unwrap();
        let s = ctx.sessions.create(ctx.now());
        ctx.sessions.update(&s.id, |x| x.user = Some(USER.into()));
        App {
            ctx,
            clock,
            cookie: format!("session={}", s.id),
            csrf: s.csrf_token,
            _dir: dir,
        }
    }

    /// A fresh sign-in at the clock's time, for a test that moved the
    /// clock past a daily boundary.
    fn sign_in_again(&mut self) {
        let s = self.ctx.sessions.create(self.ctx.now());
        self.ctx
            .sessions
            .update(&s.id, |x| x.user = Some(USER.into()));
        self.cookie = format!("session={}", s.id);
        self.csrf = s.csrf_token;
    }

    fn connect_fyers(&self) {
        BrokerAuthService::persist(
            &self.ctx,
            &BrokerSession {
                broker_id: "fyers".into(),
                auth_token: "APPID-100:access-token".into(),
                feed_token: None,
                user_id: "<USER_ID>".into(),
                user_name: None,
                authenticated_at: self.ctx.now(),
            },
        )
        .unwrap();
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ACCEPT, "application/json")
            .header(header::COOKIE, &self.cookie)
            .header("x-csrftoken", &self.csrf);
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

    /// The expiries a tool page is offered.
    async fn listed(&self, exchange: &str, underlying: &str, kind: &str) -> Value {
        let path = format!(
            "/search/api/expiries?exchange={}&underlying={}&instrumenttype={}",
            exchange,
            underlying.replace('&', "%26"),
            kind
        );
        let (s, v) = self.call(Method::GET, &path, None).await;
        assert_eq!(s, StatusCode::OK, "{} {}", path, v);
        v
    }
}

/// Poll an observable condition with a bound.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {}", what);
}

/// T01: raw Fyers master -> native parser -> master snapshot -> the
/// signed-in session route the Option Chain calls. The dates appear, and
/// they are the live ones; every tool page sends the same request.
#[tokio::test]
async fn fyers_master_fills_the_option_chain_expiry_dropdown() {
    let mut app = App::fyers(ist(2026, 10, 12, 10, 0)).await;

    // Before any master is loaded the page is told so, not handed an
    // empty calendar.
    assert_eq!(
        app.listed("NFO", "NIFTY", "options").await,
        golden(MASTER_NOT_READY)
    );

    // The download the Master Contract page starts, through the adapter.
    app.connect_fyers();
    let (s, v) = app
        .call(
            Method::POST,
            "/api/master-contract/download",
            Some(json!({"force": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    until("the Fyers master", || app.ctx.symbol_count() > 0).await;

    // The snapshot keeps Fyers' rows as the web stores them: `name` is the
    // contract description, which is why it cannot be the identity.
    let snap = app.ctx.symbols.snapshot();
    let row = snap.by_symbol("NFO", "NIFTY13OCT2625000CE").unwrap();
    assert_eq!(row.name, "NIFTY 13 Oct 26 25000 CE");
    assert_eq!(row.expiry, "13-OCT-26");
    drop(snap);

    // The Option Chain's request, answered with the live dates (06 Oct has
    // expired), earliest first: the page selects the 13th and starts the
    // chain.
    assert_eq!(
        app.listed("NFO", "NIFTY", "options").await,
        golden(FYERS_NIFTY_EXPIRIES)
    );
    let ok = |dates: &[&str]| json!({"status": "success", "expiries": dates});
    assert_eq!(
        app.listed("NFO", "NIFTY", "futures").await,
        ok(&["27-OCT-26"])
    );
    // Identity edges (T02): exact underlyings only.
    assert_eq!(
        app.listed("NFO", "NIFTYNXT50", "options").await,
        ok(&["27-OCT-26"])
    );
    assert_eq!(
        app.listed("NFO", "BANKNIFTY", "options").await,
        ok(&["27-OCT-26", "24-NOV-26"])
    );
    assert_eq!(
        app.listed("NFO", "FINNIFTY", "options").await,
        ok(&["27-OCT-26"])
    );
    for und in ["M&M", "BAJAJ-AUTO", "360ONE"] {
        assert_eq!(
            app.listed("NFO", und, "options").await,
            ok(&["27-OCT-26"]),
            "{}",
            und
        );
    }
    assert_eq!(app.listed("NFO", "M", "options").await, ok(&[]));
    // Other venues keep their own calendars; MCX options and futures
    // differ (15th against 19th).
    assert_eq!(
        app.listed("BFO", "SENSEX", "options").await,
        ok(&["15-OCT-26"])
    );
    assert_eq!(
        app.listed("BFO", "SENSEX50", "options").await,
        ok(&["16-OCT-26"])
    );
    assert_eq!(
        app.listed("BFO", "SENSEX", "futures").await,
        ok(&["03-NOV-26"])
    );
    assert_eq!(
        app.listed("CDS", "USDINR", "options").await,
        ok(&["27-OCT-26"])
    );
    assert_eq!(
        app.listed("MCX", "CRUDEOIL", "options").await,
        ok(&["15-OCT-26"])
    );
    assert_eq!(
        app.listed("MCX", "CRUDEOIL", "futures").await,
        ok(&["19-OCT-26"])
    );
    let (_, v) = app
        .call(Method::GET, "/search/api/underlyings?exchange=NFO", None)
        .await;
    assert_eq!(
        v["underlyings"],
        json!([
            "360ONE",
            "BAJAJ-AUTO",
            "BANKNIFTY",
            "FINNIFTY",
            "M&M",
            "NIFTY",
            "NIFTYNXT50"
        ])
    );

    // T04 on the same running app: the 13th stays listed until NFO's
    // 15:30 cut-off that day, then drops out without a restart.
    app.clock.set(ist(2026, 10, 13, 15, 29));
    // A new day: the trader signed in again after the 03:00 boundary (a
    // login from before it is not a current one, web e2bdd061e).
    app.sign_in_again();
    assert_eq!(
        app.listed("NFO", "NIFTY", "options").await["expiries"][0],
        "13-OCT-26"
    );
    app.clock.set(ist(2026, 10, 13, 15, 30));
    assert_eq!(
        app.listed("NFO", "NIFTY", "options").await,
        ok(&["20-OCT-26", "27-OCT-26", "03-NOV-26"])
    );
    // MCX lists its option until 23:30 on the 15th.
    app.clock.set(ist(2026, 10, 15, 23, 29));
    app.sign_in_again();
    assert_eq!(
        app.listed("MCX", "CRUDEOIL", "options").await,
        ok(&["15-OCT-26"])
    );
    app.clock.set(ist(2026, 10, 15, 23, 30));
    assert_eq!(app.listed("MCX", "CRUDEOIL", "options").await, ok(&[]));
}

// ------------------------------------------------- every parser's names

/// One expected calendar: venue, underlying, `options`/`futures`, dates.
type Case = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
);

/// The same identity on every surface that lists expiries: the pages'
/// session discovery, the public `/api/v1/expiry` and the strategy
/// module's resolver index.
fn assert_calendars(broker: &str, rows: Vec<SymToken>, now: DateTime<Utc>, cases: &[Case]) {
    let today = now.with_timezone(&Kolkata).date_naive();
    let g = SymbolGeneration::build(rows, 1);
    for (ex, und, kind, want) in cases {
        let got = expiries(g.rows(), Some(*ex), Some(*und), Some(*kind), now);
        assert_eq!(got, *want, "{} {} {} {} (pages)", broker, ex, und, kind);
        let api = expiry_dates(g.rows(), und, ex, *kind == "futures", today);
        assert_eq!(
            api, *want,
            "{} {} {} {} (/api/v1/expiry)",
            broker, ex, und, kind
        );
        let mut index: Vec<String> = if *kind == "futures" {
            g.expiries(ex, und, Some("FUT"))
        } else {
            let mut v = g.expiries(ex, und, Some("CE"));
            v.extend(g.expiries(ex, und, Some("PE")));
            v
        };
        // The resolver's index keeps expired rows; it filters by date itself.
        index.retain(|e| listed_live(e, ex, now));
        index.sort_by_key(|e| parse_oa_expiry(e).unwrap_or(NaiveDate::MAX));
        index.dedup();
        assert_eq!(
            index, *want,
            "{} {} {} {} (strategy)",
            broker, ex, und, kind
        );
    }
}

fn oct_5() -> DateTime<Utc> {
    ist(2026, 10, 5, 10, 0)
}

/// T02: each broker's own master file, through its own parser. Their
/// `name` columns differ (Fyers a description, Upstox a company name,
/// AliceBlue futures without `DDMMMYY` in the symbol, Shoonya BFO the
/// leading letters); the calendars must not.
#[test]
fn every_brokers_master_lists_the_same_calendars() {
    use openalgo_desktop_lib::brokers;
    assert_calendars(
        "fyers",
        fyers_rows(),
        ist(2026, 10, 12, 10, 0),
        &[
            (
                "NFO",
                "NIFTY",
                "options",
                &["13-OCT-26", "20-OCT-26", "27-OCT-26", "03-NOV-26"],
            ),
            ("NFO", "NIFTY", "futures", &["27-OCT-26"]),
            ("NFO", "NIFTYNXT50", "options", &["27-OCT-26"]),
            ("NFO", "M&M", "futures", &["27-OCT-26"]),
            ("BFO", "SENSEX50", "options", &["16-OCT-26"]),
            ("CDS", "USDINR", "futures", &["27-OCT-26"]),
            ("MCX", "CRUDEOIL", "options", &["15-OCT-26"]),
            ("MCX", "CRUDEOIL", "futures", &["19-OCT-26"]),
        ],
    );
    assert_calendars(
        "zerodha",
        brokers::zerodha::master_contract::parse_instruments(fx!("zerodha/instruments.csv"))
            .unwrap(),
        oct_5(),
        &[
            ("NFO", "NIFTY", "options", &["06-OCT-26"]),
            ("NFO", "NIFTY", "futures", &["27-OCT-26"]),
            ("NFO", "VEDL", "options", &["27-OCT-26"]),
            ("MCX", "CRUDEOIL", "options", &["15-OCT-26"]),
            ("MCX", "CRUDEOIL", "futures", &["19-OCT-26"]),
            ("MCX", "MCXBULLDEX", "futures", &["27-OCT-26", "24-NOV-26"]),
            ("CDS", "USDINR", "futures", &["27-OCT-26"]),
        ],
    );
    assert_calendars(
        "upstox",
        brokers::upstox::master_contract::parse_json(fx!("upstox/instruments.json").as_bytes())
            .unwrap(),
        oct_5(),
        &[
            ("NFO", "NIFTY", "options", &["06-OCT-26"]),
            ("NFO", "NIFTY", "futures", &["27-OCT-26"]),
            // `name` is "VEDANTA LIMITED".
            ("NFO", "VEDL", "options", &["27-OCT-26"]),
            ("BFO", "SENSEX", "options", &["29-OCT-26"]),
            ("CDS", "USDINR", "futures", &["27-OCT-26"]),
            // `name` is "CRUDE OIL".
            ("MCX", "CRUDEOIL", "futures", &["19-OCT-26"]),
        ],
    );
    assert_calendars(
        "angel",
        brokers::angel::master_contract::parse_master(fx!("angel/master.json").as_bytes()).unwrap(),
        oct_5(),
        &[
            ("NFO", "NIFTY", "options", &["27-OCT-26"]),
            ("NFO", "NIFTY", "futures", &["27-OCT-26"]),
            ("NFO", "VEDL", "options", &["27-OCT-26"]),
            ("BFO", "SENSEX", "options", &["08-OCT-26"]),
            ("BFO", "SENSEX", "futures", &["29-OCT-26"]),
            ("BFO", "RELIANCE", "options", &["29-OCT-26"]),
            ("MCX", "CRUDEOIL", "options", &["15-OCT-26"]),
            ("MCX", "CRUDEOIL", "futures", &["19-OCT-26"]),
            ("CDS", "USDINR", "futures", &["27-OCT-26"]),
        ],
    );
    assert_calendars(
        "dhan",
        brokers::dhan::master_contract::parse_scrip_master(
            fx!("dhan/api-scrip-master.csv"),
            brokers::dhan::Variant::Live,
        )
        .unwrap(),
        oct_5(),
        &[
            ("NFO", "NIFTY", "options", &["27-OCT-26"]),
            ("NFO", "NIFTY", "futures", &["27-OCT-26"]),
            ("NFO", "RELIANCE", "options", &["27-OCT-26"]),
            ("NFO", "TCS", "options", &["27-OCT-26"]),
            ("NFO", "VEDL", "futures", &["27-OCT-26"]),
            ("BFO", "SENSEX", "options", &["29-OCT-26"]),
            ("CDS", "USDINR", "options", &["28-OCT-26"]),
            ("MCX", "CRUDEOIL", "options", &["15-OCT-26"]),
            ("MCX", "CRUDEOIL", "futures", &["19-OCT-26"]),
        ],
    );
    let mut kotak = Vec::new();
    for (key, text) in [
        ("NSE_FO", fx!("kotak/nse_fo.csv")),
        ("BSE_FO", fx!("kotak/bse_fo.csv")),
        ("MCX_FO", fx!("kotak/mcx_fo.csv")),
        ("CDE_FO", fx!("kotak/cde_fo.csv")),
    ] {
        kotak.extend(brokers::kotak::master_contract::parse_file(key, text).unwrap());
    }
    assert_calendars(
        "kotak",
        kotak,
        oct_5(),
        &[
            ("NFO", "NIFTY", "options", &["27-OCT-26"]),
            ("NFO", "NIFTY", "futures", &["27-OCT-26"]),
            ("NFO", "VEDL", "options", &["27-OCT-26"]),
            ("BFO", "SENSEX", "options", &["29-OCT-26"]),
            ("BFO", "SENSEX", "futures", &["29-OCT-26"]),
            ("MCX", "CRUDEOIL", "options", &["19-OCT-26"]),
            ("CDS", "USDINR", "options", &["28-OCT-26"]),
        ],
    );
    let noren = |cfg, files: &[(&str, &str)]| {
        let mut rows = Vec::new();
        for (ex, text) in files {
            rows.extend(brokers::families::noren::master_contract::parse_file(
                cfg, ex, text,
            ));
        }
        rows
    };
    assert_calendars(
        "flattrade",
        noren(
            brokers::flattrade::config(),
            &[
                ("NFO", fx!("flattrade/Nfo_Index_Derivatives.csv")),
                ("NFO", fx!("flattrade/Nfo_Equity_Derivatives.csv")),
                ("MCX", fx!("flattrade/Commodity.csv")),
            ],
        ),
        oct_5(),
        &[
            ("NFO", "NIFTY", "options", &["27-OCT-26"]),
            ("NFO", "NIFTY", "futures", &["27-OCT-26"]),
            ("NFO", "SBIN", "options", &["27-OCT-26"]),
            ("NFO", "SBIN", "futures", &["27-OCT-26"]),
            ("MCX", "CRUDEOIL", "futures", &["19-OCT-26"]),
        ],
    );
    assert_calendars(
        "shoonya",
        noren(
            brokers::shoonya::config(),
            &[
                ("NFO", fx!("shoonya/NFO_symbols.txt")),
                ("MCX", fx!("shoonya/MCX_symbols.txt")),
                ("BFO", fx!("shoonya/BFO_symbols.txt")),
            ],
        ),
        oct_5(),
        &[
            ("NFO", "NIFTY", "options", &["27-OCT-26"]),
            ("NFO", "NIFTY", "futures", &["27-OCT-26"]),
            ("NFO", "VEDL", "options", &["27-OCT-26"]),
            ("MCX", "CRUDEOIL", "options", &["15-OCT-26"]),
            ("MCX", "CRUDEOIL", "futures", &["19-OCT-26"]),
            ("BFO", "SENSEX", "options", &["29-OCT-26"]),
        ],
    );
    let mut xts = brokers::families::xts::master_contract::parse_segment(
        "NSEFO",
        fx!("fivepaisaxts/NSEFO.txt"),
    );
    xts.extend(brokers::families::xts::master_contract::parse_segment(
        "MCXFO",
        fx!("fivepaisaxts/MCXFO.txt"),
    ));
    assert_calendars(
        "fivepaisaxts",
        xts,
        ist(2024, 4, 1, 10, 0),
        &[
            ("NFO", "NIFTY", "options", &["25-APR-24"]),
            ("NFO", "BANKNIFTY", "futures", &["24-APR-24"]),
            ("NFO", "VEDL", "options", &["25-APR-24"]),
            ("MCX", "CRUDEOIL", "futures", &["20-MAY-24"]),
        ],
    );
    let mut alice =
        brokers::aliceblue::master_contract::parse_exchange_csv("NFO", fx!("aliceblue/NFO.csv"));
    alice.extend(brokers::aliceblue::master_contract::parse_exchange_csv(
        "MCX",
        fx!("aliceblue/MCX.csv"),
    ));
    assert_calendars(
        "aliceblue",
        alice,
        oct_5(),
        &[
            ("NFO", "NIFTY", "options", &["28-OCT-26"]),
            // `BANKNIFTY26OCTFUT`: no DDMMMYY in AliceBlue's future symbol.
            ("NFO", "BANKNIFTY", "futures", &["28-OCT-26"]),
            ("MCX", "CRUDEOIL", "futures", &["19-NOV-26"]),
            ("MCX", "GOLD", "options", &["25-NOV-26"]),
        ],
    );
}
