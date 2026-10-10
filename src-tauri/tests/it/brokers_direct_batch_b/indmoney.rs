//! INDmoney (INDstocks) adapter suite against a local fake broker on an
//! ephemeral port: sign-in paths, raw `Authorization` header, order bodies
//! and endpoint routing, books normalised to OpenAlgo symbols, funds,
//! margin, quotes with poison-scrip bisection, depth, chunked history,
//! master contract, and order writes never retried on 429.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::indmoney::master_contract::parse_csv;
use openalgo_desktop_lib::brokers::indmoney::{Endpoints, IndmoneyBroker};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../fixtures/brokers/indmoney/", $name))
    };
}

fn responses() -> Value {
    serde_json::from_str(fixture!("responses.json")).unwrap()
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: HashMap<String, String>,
    authorization: String,
    api_key: String,
    body: Value,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    /// Answer order writes with 429.
    throttle_orders: AtomicUsize,
    /// Answer /user/profile with this status (0 = 200).
    profile_status: AtomicUsize,
    /// Answer the F&O instrument list with 500.
    fno_down: std::sync::atomic::AtomicBool,
}

impl Fake {
    fn calls(&self, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.path == path)
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

fn status(code: u16, v: Value) -> Response {
    (
        StatusCode::from_u16(code).unwrap(),
        [("content-type", "application/json")],
        v.to_string(),
    )
        .into_response()
}

const POISON: &str = "NFO_51013";

fn route(fake: &Fake, s: &Seen) -> Response {
    let all = responses();
    let pick = |k: &str| ok(&all[k]);
    if s.authorization.is_empty() && s.path != "/generate/token" {
        return status(401, json!({"message": "Unauthorized"}));
    }
    match (s.method.as_str(), s.path.as_str()) {
        ("POST", "/generate/token") => {
            if s.body["totp"] == "000000" {
                status(401, all["token_bad"].clone())
            } else {
                pick("token_ok")
            }
        }
        ("GET", "/user/profile") => match fake.profile_status.load(Ordering::SeqCst) {
            0 => pick("profile_ok"),
            c => status(c as u16, all["profile_rejected"].clone()),
        },
        ("POST", "/order") | ("POST", "/smart/order") => {
            if fake.throttle_orders.load(Ordering::SeqCst) > 0 {
                return status(429, json!({"message": "Too many requests"}));
            }
            if s.path == "/smart/order" {
                ok(
                    json!({"status":"success","data":{"order_data":[{"order_id":"GTT-42","child_order_details":{"order_id":"EQ-43"}}]}}),
                )
            } else {
                ok(json!({"status":"success","data":{"order_id":"EQ-96057848"}}))
            }
        }
        ("POST", "/order/cancel") | ("POST", "/smart/order/cancel") => {
            if s.body["order_id"] == "EQ-888" {
                ok(json!({"status":"failure","error":{"msg":"Order already rejected"}}))
            } else {
                ok(json!({"status":"success","data":{}}))
            }
        }
        ("POST", "/order/modify") | ("POST", "/smart/order/modify") => {
            ok(json!({"status":"success","data":{}}))
        }
        ("GET", "/order-book") => pick("order_book"),
        ("GET", "/trade-book") => match s.query.get("segment").map(String::as_str) {
            Some("EQUITY") => pick("trade_book_equity"),
            _ => pick("trade_book_derivative"),
        },
        ("GET", "/portfolio/positions") => {
            let k = format!("positions_{}_{}", s.query["segment"], s.query["product"]);
            pick(&k)
        }
        ("GET", "/portfolio/holdings") => pick("holdings"),
        ("GET", "/funds") => pick("funds"),
        ("GET", "/margin") => pick("margin_ok"),
        ("GET", "/market/quotes/ltp") => pick("ltp"),
        ("GET", "/market/quotes/mkt") => pick("quotes_mkt"),
        ("GET", "/market/quotes/full") => {
            let codes = s.query.get("scrip-codes").cloned().unwrap_or_default();
            if codes.split(',').any(|c| c == POISON) {
                return (StatusCode::BAD_REQUEST, "Invalid scrip codes or mode").into_response();
            }
            let full = &all["quotes_full"]["data"];
            let data: serde_json::Map<String, Value> = codes
                .split(',')
                .filter_map(|c| full.get(c).map(|q| (c.to_string(), q.clone())))
                .collect();
            ok(json!({"status":"success","data":data}))
        }
        ("GET", p) if p.starts_with("/market/historical/") => pick("history"),
        ("GET", "/market/instruments") => {
            if s.query.get("source").map(String::as_str) == Some("fno")
                && fake.fno_down.load(std::sync::atomic::Ordering::SeqCst)
            {
                return status(500, json!({"message": "down"}));
            }
            let csv = match s.query.get("source").map(String::as_str) {
                Some("equity") => fixture!("equity.csv"),
                Some("fno") => fixture!("fno.csv"),
                _ => fixture!("index.csv"),
            };
            (StatusCode::OK, csv).into_response()
        }
        _ => status(404, json!({"message":"no such endpoint"})),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let h = |k: &str| {
                    headers
                        .get(k)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string()
                };
                let query: HashMap<String, String> = uri
                    .query()
                    .map(|q| serde_urlencoded::from_str(q).unwrap_or_default())
                    .unwrap_or_default();
                let seen = Seen {
                    method: method.to_string(),
                    path: uri.path().to_string(),
                    query,
                    authorization: h("authorization"),
                    api_key: h("x-api-key"),
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                };
                fake.seen.lock().push(seen.clone());
                route(&fake, &seen)
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

fn master() -> SymbolResolver {
    let mut rows = parse_csv("equity", fixture!("equity.csv"));
    rows.extend(parse_csv("fno", fixture!("fno.csv")));
    rows.extend(parse_csv("index", fixture!("index.csv")));
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

async fn broker() -> (IndmoneyBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let b = IndmoneyBroker::with_endpoints(
        master(),
        Endpoints {
            api: host,
            prices_ws: "ws://127.0.0.1:1".into(),
            orders_ws: "ws://127.0.0.1:1".into(),
        },
    );
    (b, fake, AuthToken::new("ind-tok-123"))
}

fn order(
    symbol: &str,
    exchange: &str,
    side: &str,
    pricetype: &str,
    price: f64,
    trig: f64,
) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: side.into(),
        quantity: 10,
        price,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: Some(trig),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[tokio::test]
async fn sign_in_with_totp_pasted_token_and_fallback() {
    let (b, fake, _) = broker().await;
    let totp = BrokerCredentials {
        api_key: "CLIENT1".into(),
        password: Some("1234".into()),
        totp: Some("012345".into()),
        ..Default::default()
    };
    let r = b.authenticate(totp.clone()).await.unwrap();
    assert_eq!(
        (r.auth_token.as_str(), r.user_id.as_str()),
        ("ind-tok-123", "CLIENT1")
    );
    let call = &fake.calls("/generate/token")[0];
    assert_eq!(call.api_key, "CLIENT1");
    // Leading zero kept: the TOTP goes out as a string.
    assert_eq!(call.body, json!({"mpin":"1234","totp":"012345"}));
    // A wrong code is refused once, with the lockout hint, never retried.
    let bad = BrokerCredentials {
        totp: Some("000000".into()),
        ..totp.clone()
    };
    let e = b.authenticate(bad).await.unwrap_err();
    assert!(e.client_message().contains("Invalid TOTP"));
    assert_eq!(fake.calls("/generate/token").len(), 2);

    // Pasted token: validated with /user/profile using the raw token.
    let pasted = BrokerCredentials {
        api_key: "CLIENT1".into(),
        api_secret: Some("pasted-tok".into()),
        ..Default::default()
    };
    let r = b.authenticate(pasted.clone()).await.unwrap();
    assert_eq!(r.auth_token, "pasted-tok");
    assert_eq!(fake.calls("/user/profile")[0].authorization, "pasted-tok");
    // Unverifiable (503): the token is used anyway.
    fake.profile_status.store(503, Ordering::SeqCst);
    assert_eq!(
        b.authenticate(pasted.clone()).await.unwrap().auth_token,
        "pasted-tok"
    );
    // Rejected: without MPIN/TOTP the trader is told to log in again ...
    fake.profile_status.store(401, Ordering::SeqCst);
    let e = b.authenticate(pasted.clone()).await.unwrap_err();
    assert!(e.client_message().contains("expired"));
    // ... and with them the TOTP flow takes over.
    let both = BrokerCredentials {
        password: Some("1234".into()),
        totp: Some("123456".into()),
        ..pasted
    };
    assert_eq!(
        b.authenticate(both).await.unwrap().auth_token,
        "ind-tok-123"
    );
}

#[tokio::test]
async fn orders_route_to_the_right_endpoints() {
    let (b, fake, auth) = broker().await;
    // MARKET: priced off the full quote (812.40 * 1.001).
    let r = b
        .place_order(&auth, &order("SBIN", "NSE", "BUY", "MARKET", 0.0, 0.0))
        .await
        .unwrap();
    assert_eq!(r.order_id, "EQ-96057848");
    let p = &fake.calls("/order")[0];
    assert_eq!(p.authorization, "ind-tok-123");
    assert_eq!(p.body["order_type"], "LIMIT");
    assert_eq!(p.body["limit_price"], json!(813.21));
    assert_eq!(p.body["security_id"], "3045");
    // SL-M -> /smart/order TRIGGER.
    let r = b
        .place_order(
            &auth,
            &order("RELIANCE", "NSE", "SELL", "SL-M", 0.0, 1420.0),
        )
        .await
        .unwrap();
    assert_eq!(r.order_id, "GTT-42");
    let s = &fake.calls("/smart/order")[0];
    assert_eq!(
        (
            s.body["order_type"].as_str(),
            s.body["trigger_price"].as_f64()
        ),
        (Some("TRIGGER"), Some(1420.0))
    );
    // Modify/cancel of a book row typed GTT_LIMIT use the smart endpoints.
    let m = ResolvedModify::resolve(
        "EQ-777",
        &ModifyOrderRequest {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "SELL".into(),
            product: "CNC".into(),
            pricetype: "SL".into(),
            quantity: 5,
            price: 1417.0,
            trigger_price: 1419.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    b.modify_order(&auth, &m).await.unwrap();
    assert_eq!(
        fake.calls("/smart/order/modify")[0].body["trigger_limit_price"],
        json!(1417.0)
    );
    b.cancel_order(&auth, "EQ-96057848").await.unwrap();
    assert_eq!(
        fake.calls("/order/cancel")[0].body,
        json!({"segment":"EQUITY","order_id":"EQ-96057848"})
    );
    let e = b.cancel_order(&auth, "EQ-888").await.unwrap_err();
    assert_eq!(e.client_message(), "Order already rejected");
    // Cancel all: the open and trigger-pending rows, one book read.
    let books_before = fake.calls("/order-book").len();
    let c = b.cancel_all_orders(&auth).await.unwrap();
    assert_eq!(c.cancelled, ["EQ-96057848", "EQ-777"]);
    assert!(c.failed.is_empty());
    assert_eq!(fake.calls("/order-book").len(), books_before + 1);
    assert_eq!(fake.calls("/smart/order/cancel").len(), 1);
}

#[tokio::test]
async fn order_writes_are_never_retried_on_429() {
    let (b, fake, auth) = broker().await;
    fake.throttle_orders.store(1, Ordering::SeqCst);
    let e = b
        .place_order(&auth, &order("SBIN", "NSE", "BUY", "LIMIT", 812.0, 0.0))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("not resent"));
    assert_eq!(fake.calls("/order").len(), 1);
}

#[tokio::test]
async fn books_are_normalised() {
    let (b, fake, auth) = broker().await;
    let o = b.get_order_book(&auth).await.unwrap();
    assert_eq!(o.len(), 4);
    assert_eq!(
        (o[1].symbol.as_str(), o[1].exchange.as_str()),
        ("NIFTY27OCT2625000CE", "NFO")
    );
    let t = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!((t[0].symbol.as_str(), t[0].side.as_str()), ("SBIN", "BUY"));
    assert_eq!(fake.calls("/trade-book").len(), 2);
    let p = b.get_positions(&auth).await.unwrap();
    assert_eq!(p.len(), 3);
    assert_eq!(fake.calls("/portfolio/positions").len(), 4);
    let nifty = p.iter().find(|x| x.exchange == "NFO").unwrap();
    assert_eq!(
        (nifty.ltp, nifty.pnl, nifty.product.as_str()),
        (1240.5, 750.0, "NRML")
    );
    let sbin = p.iter().find(|x| x.symbol == "SBIN").unwrap();
    // 100 realized + (815 - 812.5) * 4.
    assert_eq!(
        (sbin.ltp, sbin.pnl, sbin.product.as_str()),
        (815.0, 110.0, "MIS")
    );
    let ltp = &fake.calls("/market/quotes/ltp")[0];
    assert_eq!(ltp.query["scrip-codes"], "NFO_51012,NSE_3045");
    let h = b.get_holdings(&auth).await.unwrap();
    assert_eq!(
        (h[0].symbol.as_str(), h[0].exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    // Open position: token and exchange match, product ignored like the web.
    let q = b
        .get_open_position(&auth, "NIFTY27OCT2625000CE", Exchange::Nfo, Product::Mis)
        .await
        .unwrap();
    assert_eq!(q, -75);
    let q = b
        .get_open_position(&auth, "TCS", Exchange::Nse, Product::Mis)
        .await
        .unwrap();
    assert_eq!(q, 0);
    // Close all: one MARKET exit per non-zero row.
    let before = fake.calls("/order").len();
    let r = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(r.placed.len(), 2);
    assert_eq!(fake.calls("/order").len(), before + 2);
    let exits: Vec<Seen> = fake.calls("/order")[before..].to_vec();
    assert!(exits.iter().any(|e| e.body["txn_type"] == "BUY"
        && e.body["segment"] == "DERIVATIVE"
        && e.body["product"] == "MARGIN"));
    assert!(exits
        .iter()
        .any(|e| e.body["txn_type"] == "SELL" && e.body["security_id"] == "3045"));
}

#[tokio::test]
async fn funds_and_margin() {
    let (b, fake, auth) = broker().await;
    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!((f.available_cash, f.utilised_debits), (2980.4, 2019.6));
    let legs = vec![
        MarginLeg {
            key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
            action: openalgo_desktop_lib::brokers::common::mapping::Action::Sell,
            quantity: 75,
            product: Product::Nrml,
            pricetype: openalgo_desktop_lib::brokers::common::mapping::PriceType::Market,
            price: 0.0,
            trigger_price: 0.0,
        };
        2
    ];
    let m = b.calculate_margin(&auth, &legs).await.unwrap();
    assert_eq!(m.total_margin_required, 240001.0);
    let calls = fake.calls("/margin");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].method, "GET");
    assert_eq!(calls[0].body["securityID"], "51012");
    let unknown = vec![MarginLeg {
        key: QuoteKey::new("NSE", "NOPE"),
        ..legs[0].clone()
    }];
    assert!(b.calculate_margin(&auth, &unknown).await.is_err());
}

#[tokio::test]
async fn quotes_bisect_a_poisoned_batch() {
    let (b, fake, auth) = broker().await;
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((q.ltp, q.bid, q.close), (812.4, 812.35, 808.0));
    let keys = [
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NFO", "VEDL27OCT26292.5PE"),
        QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        QuoteKey::new("NSE_INDEX", "NIFTY"),
        QuoteKey::new("NSE", "NOPE"),
    ];
    let r = b.get_multiquotes(&auth, &keys).await.unwrap();
    assert_eq!(r[0].data.as_ref().unwrap().ltp, 812.4);
    assert_eq!(r[0].data.as_ref().unwrap().bid, 0.0);
    assert_eq!(r[1].error.as_deref(), Some("No data received"));
    assert_eq!(r[2].data.as_ref().unwrap().oi, 123450);
    assert_eq!(r[3].data.as_ref().unwrap().ltp, 25010.5);
    assert!(r[4].error.as_deref().unwrap().contains("NOPE"));
    assert_eq!(b.bad_scrip_count(), 1);
    // The poisoned code is now skipped: one request for the next batch.
    let before = fake.calls("/market/quotes/full").len();
    b.get_multiquotes(&auth, &keys).await.unwrap();
    let after = fake.calls("/market/quotes/full");
    assert_eq!(after.len(), before + 1);
    assert!(!after.last().unwrap().query["scrip-codes"].contains(POISON));
    // Depth.
    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(
        (d.bids[1].quantity, d.total_buy_qty, d.ltp),
        (1200, 12000, 812.4)
    );
}

#[tokio::test]
async fn history_is_chunked_and_deduplicated() {
    let (b, fake, auth) = broker().await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(),
    };
    let c = b.get_history(&auth, &req).await.unwrap();
    let calls = fake.calls("/market/historical/5minute");
    // 20 days in 7-day windows.
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].query["scrip-codes"], "NSE_3045");
    // 2026-09-01 00:00 IST in epoch ms.
    assert_eq!(calls[0].query["start_time"], "1788201000000");
    assert_eq!(c.len(), 2);
    assert!(c[0].timestamp < c[1].timestamp);
    let day = HistoryRequest {
        interval: "D".into(),
        start: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        ..req.clone()
    };
    b.get_history(&auth, &day).await.unwrap();
    assert_eq!(fake.calls("/market/historical/1day").len(), 1);
    let bad = HistoryRequest {
        interval: "1s".into(),
        ..req
    };
    assert!(b.get_history(&auth, &bad).await.is_err());
}

#[tokio::test]
async fn master_contract_downloads_three_sources() {
    let (b, fake, auth) = broker().await;
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert_eq!(rows.len(), 15);
    let calls = fake.calls("/market/instruments");
    let sources: Vec<&str> = calls.iter().map(|c| c.query["source"].as_str()).collect();
    assert_eq!(sources, ["equity", "fno", "index"]);
    assert!(calls.iter().all(|c| c.authorization == "ind-tok-123"));
    let e = b
        .download_master_contract(&AuthToken::new(""))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("session"));

    // MC-02 (a hardening over the web, which keeps the sources it got): a
    // source that fails refuses the download by name; the stored master is
    // kept.
    fake.fno_down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let msg = b
        .download_master_contract(&auth)
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("INDmoney's fno instrument list"), "{}", msg);
    assert!(msg.contains("existing symbols were kept"), "{}", msg);
}
