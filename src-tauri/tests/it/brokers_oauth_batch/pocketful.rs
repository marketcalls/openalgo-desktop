//! Pocketful against a local fake: OAuth2 code exchange with Basic client
//! auth and the trading_info lookup, an expired or account-less session,
//! quotes, depth, multiquotes and history over REST, and the
//! master-contract ZIP. Orders, books and close-all are ported from the
//! web's tests in `brokers/pocketful/rest_tests/orders.rs`; the market data
//! cases in `rest_tests/data.rs`.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::pocketful::master_contract::parse_archive;
use openalgo_desktop_lib::brokers::pocketful::{auth, zip, PocketfulBroker};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../fixtures/brokers/pocketful/", $name))
    };
}

const FILES: [(&str, &str); 5] = [
    ("NSECompactScrip.csv", fixture!("NSECompactScrip.csv")),
    ("BSECompactScrip.csv", fixture!("BSECompactScrip.csv")),
    ("NFOCompactScrip.csv", fixture!("NFOCompactScrip.csv")),
    ("BFOCompactScrip.csv", fixture!("BFOCompactScrip.csv")),
    ("MCXCompactScrip.csv", fixture!("MCXCompactScrip.csv")),
];

fn master() -> SymbolResolver {
    let files: Vec<(String, Vec<u8>)> = FILES
        .iter()
        .map(|(n, t)| (n.to_string(), t.as_bytes().to_vec()))
        .collect();
    let r = SymbolResolver::new();
    r.load(parse_archive(&files).unwrap());
    r
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    access_header: String,
    body: String,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
}

impl Fake {
    fn calls(&self, method: &str, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.method == method && s.path == path)
            .cloned()
            .collect()
    }
}

fn ok(v: impl ToString) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        v.to_string(),
    )
        .into_response()
}

fn route(method: &Method, path: &str, body: &str, auth: &str) -> Response {
    if path == "/oauth2/token" {
        if body.contains("code=bad") {
            return (
                StatusCode::BAD_REQUEST,
                json!({"error": "invalid_grant", "message": "code expired"}).to_string(),
            )
                .into_response();
        }
        return ok(
            json!({"access_token": "acc-pf-1", "token_type": "bearer", "refresh_token": "r"}),
        );
    }
    if auth == "Bearer expired" {
        return (StatusCode::UNAUTHORIZED, "{}").into_response();
    }
    match (method.as_str(), path) {
        ("GET", "/api/v1/user/trading_info") => {
            ok(json!({"status": "success", "data": {"client_id": "DEMO1"}}))
        }
        ("GET", "/api/v2/funds/view") => ok(fixture!("funds.json")),
        // get-market-data.md and chart-data.md, verbatim.
        ("GET", "/api/v1/marketdata/NSE/Capital") => ok(fixture!("market_data.json")),
        ("GET", "/api/v1/charts") => ok(fixture!("chart_data.json")),
        ("GET", "/contract") => {
            let files: Vec<(&str, &[u8])> = FILES.iter().map(|(n, t)| (*n, t.as_bytes())).collect();
            (StatusCode::OK, zip::build(&files)).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "no such endpoint").into_response(),
    }
}

async fn serve_rest(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let body = String::from_utf8_lossy(&body).to_string();
                let header = |k: &str| {
                    headers
                        .get(k)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string()
                };
                let auth = header("authorization");
                let access_header = header("x-authorization-token");
                let path = uri.path().to_string();
                fake.seen.lock().push(Seen {
                    method: method.to_string(),
                    path: path.clone(),
                    authorization: auth.clone(),
                    access_header,
                    body: body.clone(),
                });
                route(&method, &path, &body, &auth)
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

async fn setup() -> (PocketfulBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let rest = serve_rest(fake.clone()).await;
    // No market data goes over a socket now; the feed is the streaming
    // tests' (a closed port here).
    let b = PocketfulBroker::with_base_url(
        master(),
        &rest,
        "ws://127.0.0.1:9",
        format!("{}/contract", rest),
    );
    (
        b,
        fake,
        AuthToken::new("acc-pf-1").with_user_id("<USER_ID>"),
    )
}

#[tokio::test]
async fn pocketful_login_exchanges_the_code_with_basic_auth() {
    let (b, fake, _) = setup().await;
    let creds = BrokerCredentials {
        api_key: "cid-1".into(),
        api_secret: Some("sec-1".into()),
        auth_code: Some("good-code".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    assert_eq!(r.auth_token, "acc-pf-1");
    assert_eq!(r.user_id, "DEMO1");
    assert!(r.feed_token.is_none());
    let tok = fake.calls("POST", "/oauth2/token");
    assert_eq!(tok.len(), 1);
    // base64("cid-1:sec-1")
    assert_eq!(tok[0].authorization, "Basic Y2lkLTE6c2VjLTE=");
    let form: Vec<(String, String)> = serde_urlencoded::from_str(&tok[0].body).unwrap();
    assert!(form.contains(&("grant_type".into(), "authorization_code".into())));
    assert!(form.contains(&("code".into(), "good-code".into())));
    assert!(form
        .iter()
        .any(|(k, v)| k == "redirect_uri" && v.ends_with("/pocketful/callback")));
    let info = fake.calls("GET", "/api/v1/user/trading_info");
    assert_eq!(info[0].authorization, "Bearer acc-pf-1");
    assert_eq!(info[0].access_header, "acc-pf-1");

    let bad = BrokerCredentials {
        auth_code: Some("bad".into()),
        ..creds.clone()
    };
    let e = b.authenticate(bad).await.unwrap_err();
    assert_eq!(e.client_message(), auth::CODE_REFUSED);
    let missing = BrokerCredentials {
        api_secret: None,
        ..creds
    };
    assert!(b.authenticate(missing).await.is_err());
}

/// A session without its trading account (an older build's row, a
/// damaged one) is expired: nothing is sent, the trader logs in again.
#[tokio::test]
async fn pocketful_a_session_without_its_account_is_expired() {
    let (b, fake, _) = setup().await;
    let bare = AuthToken::new("acc-pf-1");
    let e = b.get_funds(&bare).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    let e = b.get_holdings(&bare).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    // History sends the account as `clientid` (chart-data.md).
    let e = b.get_history(&bare, &sbin_daily()).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    assert!(fake.seen.lock().is_empty(), "nothing was sent");
    assert!(b.create_feed(&bare).is_err());

    let expired = AuthToken::new("expired").with_user_id("<USER_ID>");
    let e = b.get_order_book(&expired).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    let e = b.get_funds(&expired).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    let e = b.get_history(&expired, &sbin_daily()).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    let e = b
        .get_quote(&expired, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
}

fn sbin_daily() -> HistoryRequest {
    HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "D".into(),
        start: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
        end: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
    }
}

/// Market data over REST through the crate's API (web openalgo#2467):
/// the documented answers, one request per symbol in the order asked, and
/// one-minute chart candles built into a daily bar.
#[tokio::test]
async fn pocketful_quotes_depth_and_history_over_rest() {
    let (b, fake, auth) = setup().await;
    let sbin = QuoteKey::new("NSE", "SBIN");

    let q = b.get_quote(&auth, &sbin).await.unwrap();
    // get-market-data.md: a 12.90 stock, its book in paise.
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.close, q.volume),
        (12.9, 12.85, 12.9, 13.1, 178286763)
    );
    let d = b.get_market_depth(&auth, &sbin).await.unwrap();
    assert_eq!((d.bids.len(), d.asks.len()), (5, 5));
    assert_eq!((d.bids[0].price, d.bids[0].quantity), (12.85, 3486991));
    assert_eq!((d.bids[1].price, d.asks[4].quantity), (0.0, 0));
    assert_eq!((d.ltp, d.ltq), (12.9, 1));

    let res = b
        .get_multiquotes(
            &auth,
            &[
                sbin.clone(),
                QuoteKey::new("NSE", "NOPE"),
                QuoteKey::new("NSE_INDEX", "NIFTY"),
            ],
        )
        .await
        .unwrap();
    let order: Vec<&str> = res.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(order, ["SBIN", "NOPE", "NIFTY"], "in the order asked");
    assert!(res[1].data.is_none());
    assert!(res[1].error.as_deref().unwrap().contains("NOPE on NSE"));
    assert_eq!(res[2].data.as_ref().unwrap().ltp, 12.9);

    let bars = b.get_history(&auth, &sbin_daily()).await.unwrap();
    // chart-data.md's two candles, one IST date: one bar at 00:00 UTC.
    assert_eq!(bars.len(), 1);
    assert_eq!(bars[0].timestamp, 1_735_689_600);
    assert_eq!(
        (bars[0].open, bars[0].high, bars[0].low, bars[0].close),
        (138.35, 138.35, 137.4, 137.7)
    );
    assert_eq!(bars[0].volume, 16699 + 19642);

    let seen = fake.seen.lock().clone();
    let quotes = seen
        .iter()
        .filter(|s| s.method == "GET" && s.path == "/api/v1/marketdata/NSE/Capital")
        .count();
    assert_eq!(quotes, 4, "a quote, a depth, two of the three symbols");
    assert_eq!(fake.calls("GET", "/api/v1/charts").len(), 1);
    assert!(seen.iter().all(|s| s.authorization == "Bearer acc-pf-1"));
}

#[tokio::test]
async fn pocketful_master_contract_download() {
    let (b, _, auth) = setup().await;
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert_eq!(rows.len(), 26);
    let fut = rows.iter().find(|r| r.symbol == "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.brsymbol, "NIFTY26OCTFUT");
    assert_eq!(fut.token, "35003");
    assert!(rows
        .iter()
        .any(|r| r.exchange == "NSE_INDEX" && r.symbol == "BANKNIFTY"));
}
