//! A fake 5paisa XTS (ephemeral port): the interactive and market-data
//! REST calls the adapter makes, each answered from what a test sets, and
//! a log of what reached it. Market-data logins hand out numbered tokens
//! (`md_token`), so a test can tell which session a call used.

use axum::extract::{OriginalUri, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::families::xts::XtsBroker;
use openalgo_desktop_lib::brokers::fivepaisaxts::CONFIG;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::BrokerCredentials;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// A market-data token as XTS issues it: a JWT whose `userID` claim reads
/// `<user id>_<app id>`, numbered by the login that issued it. Built at
/// run time, so no token-shaped literal is in the source.
pub(super) fn md_token(n: usize) -> String {
    let part = |s: String| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    format!(
        "{}.{}.sig",
        part("{\"alg\":\"HS256\"}".into()),
        part(format!("{{\"userID\":\"1048131_APP\",\"n\":{n}}}"))
    )
}

#[derive(Default)]
pub(super) struct Fake {
    seen: Mutex<Vec<String>>,
    pub(super) logins: AtomicUsize,
    /// Market-data tokens answered "Invalid Token".
    pub(super) expired: Mutex<Vec<String>>,
    /// While set, a market-data login waits for it before answering.
    pub(super) hold_login: Mutex<Option<Arc<Notify>>>,
    pub(super) orders: Mutex<Value>,
    pub(super) trades: Mutex<Value>,
    pub(super) positions: Mutex<Value>,
    pub(super) balance: Mutex<Value>,
    /// The answer to a cancel or a modify.
    pub(super) answer: Mutex<Value>,
}

type S = State<Arc<Fake>>;

impl Fake {
    fn log(&self, line: String) {
        self.seen.lock().push(line);
    }
    pub(super) fn lines(&self, prefix: &str) -> Vec<String> {
        self.seen
            .lock()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .cloned()
            .collect()
    }
    fn refuses(&self, token: &str) -> bool {
        self.expired.lock().iter().any(|t| t == token)
    }
}

fn header(h: &HeaderMap) -> String {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn invalid_token() -> Value {
    json!({"type": "error", "code": "e-session-0007", "description": "Invalid Token"})
}

async fn md_login(State(s): S, OriginalUri(u): OriginalUri) -> Json<Value> {
    let n = s.logins.fetch_add(1, Ordering::SeqCst) + 1;
    s.log(format!("login|{}", u.path()));
    let gate = s.hold_login.lock().clone();
    if let Some(gate) = gate {
        gate.notified().await;
    }
    Json(json!({"type": "success", "result": {"token": md_token(n), "userID": "1048131"}}))
}

fn master_rows(segment: &str) -> &'static str {
    // The XTS v2 docs' example rows: an option (23 fields), a future (21)
    // and a spread (21, InstrumentType 4).
    match segment {
        "NSEFO" => concat!(
            "NSEFO|48225|2|NIFTY|NIFTY2621725700PE|OPTIDX|NIFTY-OPTIDX|2604800048225|588|114.5|1801|0.05|65|1|-1|Nifty 50|2026-02-17T14:30:00|25700|4|NIFTY 17FEB2026 PE 25700|1|1|NIFTY2621725700PE\n",
            "NSEFO|49229|1|NIFTY|NIFTY26JANFUT|FUTIDX|NIFTY-FUTIDX|2602700049229|28369.8|23211.8|1801|0.1|65|1|-1|Nifty 50|2026-01-27T14:30:00|NIFTY 27JAN2026|2|1|NIFTY26JANFUT\n",
            "NSEFO|60000|4|NIFTY|NIFTYSPD|FUTIDX|NIFTY-FUTIDX|1|1|1|1|0.1|65|1|-1|Nifty 50|2026-01-27T14:30:00|NIFTY SPD|1|1|NIFTYSPD\n",
        ),
        _ => "",
    }
}

fn md_routes(prefix: &str) -> Router<Arc<Fake>> {
    Router::new()
        .route(&format!("{prefix}/auth/login"), post(md_login))
        .route(
            &format!("{prefix}/instruments/quotes"),
            post(|State(s): S, h: HeaderMap| async move {
                let token = header(&h);
                s.log(format!("quotes|{token}"));
                if s.refuses(&token) {
                    return Json(invalid_token());
                }
                let q = json!({"ExchangeSegment": 1, "ExchangeInstrumentID": 2885,
                    "Touchline": {"LastTradedPrice": 2500.25, "Close": 2490.0}});
                Json(json!({"type": "success", "result": {"listQuotes": [q.to_string()]}}))
            }),
        )
        .route(
            &format!("{prefix}/instruments/ohlc"),
            get(
                |State(s): S, h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    let token = header(&h);
                    s.log(format!(
                        "ohlc|{}|{}|{}|{}",
                        token, q["startTime"], q["endTime"], q["compressionValue"]
                    ));
                    if s.refuses(&token) {
                        return Json(invalid_token());
                    }
                    Json(json!({"type": "success",
                        "result": {"dataReponse": "1737331200|1316|1316|1300.25|1305.45|14040244|0|"}}))
                },
            ),
        )
        .route(
            &format!("{prefix}/instruments/master"),
            post(|Json(b): Json<Value>| async move {
                let seg = b["exchangeSegmentList"][0].as_str().unwrap_or("").to_string();
                Json(json!({"type": "success", "result": master_rows(&seg)}))
            }),
        )
        .route(
            &format!("{prefix}/instruments/indexlist"),
            get(|| async { Json(json!({"type": "success", "result": {"indexList": []}})) }),
        )
        .route(
            &format!("{prefix}/instruments/subscription"),
            post(|| async { Json(json!({"type": "success", "result": {"listQuotes": []}})) }),
        )
}

fn app(state: Arc<Fake>) -> Router {
    Router::new()
        .route(
            "/interactive/user/session",
            post(|| async {
                Json(json!({"type": "success", "result": {"token": "itok", "userID": "1048131"}}))
            }),
        )
        .route(
            "/interactive/orders",
            get(|State(s): S| async move { Json(s.orders.lock().clone()) })
                .put(|State(s): S| async move { Json(s.answer.lock().clone()) })
                .delete(|State(s): S, OriginalUri(u): OriginalUri| async move {
                    s.log(format!("cancel|{}", u.query().unwrap_or("")));
                    Json(s.answer.lock().clone())
                }),
        )
        .route(
            "/interactive/orders/trades",
            get(|State(s): S| async move { Json(s.trades.lock().clone()) }),
        )
        .route(
            "/interactive/portfolio/positions",
            get(|State(s): S| async move { Json(s.positions.lock().clone()) }),
        )
        .route(
            "/interactive/user/balance",
            get(|State(s): S| async move { Json(s.balance.lock().clone()) }),
        )
        .merge(md_routes("/apimarketdata"))
        .merge(md_routes("/apibinarymarketdata"))
        .with_state(state)
}

pub(super) async fn serve(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn row(symbol: &str, exchange: &str, token: &str, brsymbol: &str, brexchange: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: brsymbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: brexchange.into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

pub(super) fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![
        row("RELIANCE", "NSE", "2885", "RELIANCE-EQ", "NSECM"),
        row("NIFTY", "NSE_INDEX", "26000", "NIFTY 50", "NSE_INDEX"),
        row(
            "NIFTY27OCT2622400CE",
            "NFO",
            "48225",
            "NIFTY26OCT22400CE",
            "NSEFO",
        ),
    ]);
    r
}

pub(super) async fn broker() -> (XtsBroker, Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let base = serve(app(fake.clone())).await;
    (
        XtsBroker::with_base_url(&CONFIG, master(), base.as_str()),
        fake,
        base,
    )
}

pub(super) fn creds() -> BrokerCredentials {
    BrokerCredentials {
        api_key: "ikey".into(),
        api_secret: Some("isecret".into()),
        api_key_market: Some("mkey".into()),
        api_secret_market: Some("msecret".into()),
        ..Default::default()
    }
}

pub(super) fn token() -> AuthToken {
    AuthToken::new("itok").with_feed(Some(md_token(1)))
}

pub(super) async fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A history request for RELIANCE on NSE.
pub(super) fn history(
    interval: &str,
    start: (i32, u32, u32),
    end: (i32, u32, u32),
) -> HistoryRequest {
    let day = |(y, m, d): (i32, u32, u32)| NaiveDate::from_ymd_opt(y, m, d).unwrap();
    HistoryRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        interval: interval.into(),
        start: day(start),
        end: day(end),
    }
}
