//! IIFL Capital adapter suite against a local fake broker (ephemeral port):
//! sign-in checksum, master contract (and its all-or-nothing abort), order
//! bodies (SL-M protection, writes never resent), cancel-all, close-all,
//! open position, books normalised to OpenAlgo symbols, funds, margin,
//! quotes with OI, multiquotes, depth and history.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::iiflcapital::{
    auth, IiflCapitalBroker, UNCONFIRMED_WRITE_MESSAGE,
};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../fixtures/brokers/iiflcapital/", $name))
    };
}

const JWT: &str = "eyJhbGciOiJSUzI1NiJ9.eyJwcmVmZXJyZWRfdXNlcm5hbWUiOiAiQ0wxIiwgImV4cCI6IDF9.c2ln";

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    body: Value,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    order_posts: AtomicUsize,
    trades_calls: AtomicUsize,
    fail_segment: AtomicBool,
    /// The position book answers an error page.
    positions_down: AtomicBool,
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

fn csv(text: &'static str) -> Response {
    (StatusCode::OK, [("content-type", "text/csv")], text).into_response()
}

fn route(fake: &Fake, method: &Method, path: &str, body: &Value) -> Response {
    match (method.as_str(), path) {
        ("POST", "/getusersession") => {
            if body["checkSum"] == auth::checksum("778", "ac9", "SEC") {
                ok(json!({"status":"Ok","message":"Success","userSession":JWT}))
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    [("content-type", "application/json")],
                    json!({"status":"error","message":"Invalid checksum"}).to_string(),
                )
                    .into_response()
            }
        }
        ("GET", "/orders") => ok(fixture!("order_book.json")),
        ("GET", "/trades") => {
            if fake.trades_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "0.05")],
                    "slow down",
                )
                    .into_response()
            } else {
                ok(fixture!("trade_book.json"))
            }
        }
        ("GET", "/positions") if fake.positions_down.load(Ordering::SeqCst) => (
            StatusCode::BAD_GATEWAY,
            [("content-type", "text/html")],
            "<html>Bad gateway</html>",
        )
            .into_response(),
        ("GET", "/positions") => ok(fixture!("positions.json")),
        ("GET", "/holdings") => ok(fixture!("holdings.json")),
        ("GET", "/limits") => ok(fixture!("limits.json")),
        ("GET", "/limits/equity") => ok(fixture!("limits_equity.json")),
        ("GET", "/limits/fno") => ok(fixture!("limits_fno.json")),
        ("POST", "/orders") => {
            let n = fake.order_posts.fetch_add(1, Ordering::SeqCst);
            if body[0]["quantity"] == "13" {
                return (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    json!({"status":"error","message":"EC003 Something went wrong, please try after some time"})
                        .to_string(),
                )
                    .into_response();
            }
            if body[0]["quantity"] == "14" {
                return ok(
                    json!({"status":"Ok","result":[{"status":"error","message":"RMS: insufficient funds"}]}),
                );
            }
            ok(json!({"status":"Ok","result":[{"status":"Ok","brokerOrderId":format!("BO{}", n)}]}))
        }
        ("POST", "/spanexposure") => {
            ok(json!({"status":"Ok","result":{"span":"12000.5","exposureMargin":"3000"}}))
        }
        ("POST", "/marketdata/marketquotes") => ok(fixture!("marketquotes.json")),
        ("POST", "/marketdata/openinterest") => ok(fixture!("openinterest.json")),
        ("POST", "/marketdata/marketdepth") => ok(fixture!("marketdepth.json")),
        ("POST", "/marketdata/historicaldata") => ok(fixture!("historical.json")),
        ("GET", p) if p.starts_with("/contractfiles/") => {
            let name = p.trim_start_matches("/contractfiles/");
            if name == "BSECURR.csv" && fake.fail_segment.load(Ordering::SeqCst) {
                return (StatusCode::BAD_GATEWAY, "upstream").into_response();
            }
            match name {
                "NSEEQ.csv" => csv(fixture!("NSEEQ.csv")),
                "BSEEQ.csv" => csv(fixture!("BSEEQ.csv")),
                "NSEFO.csv" => csv(fixture!("NSEFO.csv")),
                "BSEFO.csv" => csv(fixture!("BSEFO.csv")),
                "NSECURR.csv" => csv(fixture!("NSECURR.csv")),
                "BSECURR.csv" => csv(fixture!("BSECURR.csv")),
                "NSECOMM.csv" => csv(fixture!("NSECOMM.csv")),
                "MCXCOMM.csv" => csv(fixture!("MCXCOMM.csv")),
                "NCDEXCOMM.csv" => csv(fixture!("NCDEXCOMM.csv")),
                "INDICES.csv" => csv(fixture!("INDICES.csv")),
                _ => (StatusCode::NOT_FOUND, "").into_response(),
            }
        }
        ("PUT", p) if p.starts_with("/orders/") => {
            ok(json!({"status":"Ok","result":{"status":"Ok"}}))
        }
        ("DELETE", "/orders/260403000000103") => {
            ok(json!({"status":"error","message":"Order is already cancelled"}))
        }
        ("DELETE", p) if p.starts_with("/orders/") => ok(json!({"status":"Ok"})),
        _ => (StatusCode::NOT_FOUND, "no route").into_response(),
    }
}

async fn start() -> (Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let f = fake.clone();
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let f = f.clone();
            async move {
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let path = uri.path().to_string();
                f.seen.lock().push(Seen {
                    method: method.to_string(),
                    path: path.clone(),
                    authorization: headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string(),
                    body: body.clone(),
                });
                route(&f, &method, &path, &body)
            }
        },
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    (fake, base)
}

fn auth_token() -> AuthToken {
    AuthToken::new(JWT)
}

async fn broker_with_master() -> (Arc<Fake>, IiflCapitalBroker, SymbolResolver) {
    let (fake, base) = start().await;
    let symbols = SymbolResolver::new();
    let b = IiflCapitalBroker::with_base_url(symbols.clone(), base)
        .with_backoff(Duration::from_millis(20), Duration::from_millis(5));
    let rows = b.download_master_contract(&auth_token()).await.unwrap();
    symbols.load(rows);
    (fake, b, symbols)
}

#[allow(clippy::too_many_arguments)]
fn order(
    symbol: &str,
    exchange: &str,
    side: &str,
    qty: i32,
    ot: &str,
    price: f64,
    trig: Option<f64>,
    product: &str,
) -> OrderRequest {
    OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: side.into(),
        quantity: qty,
        price,
        order_type: ot.into(),
        product: product.into(),
        validity: "DAY".into(),
        trigger_price: trig,
        disclosed_quantity: None,
        amo: false,
    }
}

#[tokio::test]
async fn sign_in_uses_the_callback_client_id() {
    let (fake, base) = start().await;
    let b = IiflCapitalBroker::with_base_url(SymbolResolver::new(), base);
    let creds = BrokerCredentials {
        api_key: "APPKEY".into(),
        api_secret: Some("SEC".into()),
        auth_code: Some("778:::ac9".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    assert_eq!(r.auth_token, JWT);
    assert_eq!(r.user_id, "778");
    assert!(r.feed_token.is_none());
    let call = &fake.calls("POST", "/getusersession")[0];
    assert_eq!(
        call.body,
        json!({"checkSum": auth::checksum("778", "ac9", "SEC")})
    );
    // Bare code: the client id falls back to the stored `clientid:::appkey`.
    let fallback = BrokerCredentials {
        api_key: "778:::APPKEY".into(),
        auth_code: Some("ac9".into()),
        ..creds.clone()
    };
    assert_eq!(b.authenticate(fallback).await.unwrap().user_id, "778");
    let wrong = BrokerCredentials {
        api_secret: Some("NOPE".into()),
        ..creds.clone()
    };
    let e = b.authenticate(wrong).await.unwrap_err();
    assert!(
        e.client_message().contains("Invalid checksum"),
        "{}",
        e.client_message()
    );
    let no_secret = BrokerCredentials {
        api_secret: None,
        ..creds
    };
    assert!(b.authenticate(no_secret).await.is_err());
}

#[tokio::test]
async fn master_contract_downloads_every_segment_or_nothing() {
    let (fake, b, symbols) = broker_with_master().await;
    assert!(symbols.by_symbol("NFO", "NIFTY30APR2624000CE").is_some());
    assert_eq!(
        symbols.by_symbol("NSE_INDEX", "NIFTY").unwrap().token,
        "26000"
    );
    assert_eq!(fake.calls("GET", "/contractfiles/INDICES.csv").len(), 1);
    fake.fail_segment.store(true, Ordering::SeqCst);
    let e = b.download_master_contract(&auth_token()).await.unwrap_err();
    assert!(e.client_message().contains("BSECURR"));
    assert_eq!(
        fake.calls("GET", "/contractfiles/BSECURR.csv").len(),
        1 + 4,
        "four attempts on the failing segment"
    );
}

#[tokio::test]
async fn orders_are_shaped_like_the_web() {
    let (fake, b, symbols) = broker_with_master().await;
    let a = auth_token();
    let o = ResolvedOrder::resolve(
        &order("SBIN", "NSE", "BUY", 10, "LIMIT", 812.5, None, "MIS"),
        &symbols,
    )
    .unwrap();
    let r = b.place_order(&a, &o).await.unwrap();
    assert!(r.order_id.starts_with("BO"));
    let sent = fake.calls("POST", "/orders");
    assert_eq!(sent[0].authorization, format!("Bearer {}", JWT));
    assert_eq!(
        sent[0].body,
        json!([{"instrumentId":"3045","exchange":"NSEEQ","transactionType":"BUY","quantity":"10",
                "orderComplexity":"REGULAR","product":"INTRADAY","validity":"DAY",
                "apiOrderSource":"openalgo","orderType":"LIMIT","price":812.5}])
    );
    // SL-M goes out as SL with a protected limit from the master tick.
    let s = ResolvedOrder::resolve(
        &order(
            "NIFTY30APR2624000CE",
            "NFO",
            "SELL",
            75,
            "SL-M",
            0.0,
            Some(120.0),
            "NRML",
        ),
        &symbols,
    )
    .unwrap();
    b.place_order(&a, &s).await.unwrap();
    let body = &fake.calls("POST", "/orders")[1].body[0];
    assert_eq!(body["orderType"], "SL");
    assert_eq!(body["slTriggerPrice"], 120.0);
    assert_eq!(
        body["price"], 117.6,
        "2% below the trigger for an option over 100"
    );
    assert_eq!(body["exchange"], "NSEFO");

    // A throttle reply to a write is never resent.
    let before = fake.order_posts.load(Ordering::SeqCst);
    let t = ResolvedOrder::resolve(
        &order("SBIN", "NSE", "BUY", 13, "MARKET", 0.0, None, "MIS"),
        &symbols,
    )
    .unwrap();
    let e = b.place_order(&a, &t).await.unwrap_err();
    assert_eq!(e.client_message(), UNCONFIRMED_WRITE_MESSAGE);
    assert_eq!(fake.order_posts.load(Ordering::SeqCst), before + 1);
    // A refusal inside the result is an error with the broker's reason.
    let rj = ResolvedOrder::resolve(
        &order("SBIN", "NSE", "BUY", 14, "MARKET", 0.0, None, "MIS"),
        &symbols,
    )
    .unwrap();
    let e = b.place_order(&a, &rj).await.unwrap_err();
    assert!(e.client_message().contains("insufficient funds"));

    // Modify sends only the changed fields to PUT /orders/{id}.
    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "LIMIT".into(),
        quantity: 5,
        price: 810.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("260403000000103", &m, &symbols).unwrap();
    assert_eq!(
        b.modify_order(&a, &rm).await.unwrap().order_id,
        "260403000000103"
    );
    assert_eq!(
        fake.calls("PUT", "/orders/260403000000103")[0].body,
        json!({"quantity":"5","orderType":"LIMIT","price":810.0})
    );
    assert!(b.cancel_order(&a, "../x").await.is_err());
    assert!(b.cancel_order(&a, "260403000000101").await.is_ok());
    let e = b.cancel_order(&a, "260403000000103").await.unwrap_err();
    assert!(e.client_message().contains("already cancelled"));
}

#[tokio::test]
async fn cancel_all_close_all_and_open_position() {
    let (fake, b, _symbols) = broker_with_master().await;
    let a = auth_token();
    let r = b.cancel_all_orders(&a).await.unwrap();
    assert_eq!(r.cancelled, ["260403000000102"]);
    assert_eq!(r.failed, ["260403000000103"]);
    assert!(fake.calls("DELETE", "/orders/260403000000101").is_empty());

    let c = b.close_all_positions(&a).await.unwrap();
    assert_eq!(c.placed.len(), 2);
    assert!(c.failed.is_empty());
    let exits: Vec<Value> = fake
        .calls("POST", "/orders")
        .iter()
        .map(|s| s.body[0].clone())
        .collect();
    assert_eq!(exits[0]["transactionType"], "SELL");
    assert_eq!(exits[0]["quantity"], "10");
    assert_eq!(exits[0]["orderTag"], "close_all_positions");
    assert_eq!(exits[1]["transactionType"], "BUY");
    assert_eq!(exits[1]["quantity"], "75");
    assert_eq!(exits[1]["product"], "NORMAL");
    assert_eq!(c.message(), "All Open Positions SquaredOff");

    // BR-03: a position book that could not be read is an error, not "no
    // positions", and no exit is sent.
    fake.positions_down.store(true, Ordering::SeqCst);
    let before = fake.calls("POST", "/orders").len();
    let e = b.close_all_positions(&a).await.unwrap_err();
    assert!(
        e.client_message()
            .contains("could not return your positions"),
        "{}",
        e.client_message()
    );
    assert_eq!(fake.calls("POST", "/orders").len(), before);
    fake.positions_down.store(false, Ordering::SeqCst);

    assert_eq!(
        b.get_open_position(&a, "SBIN", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        10
    );
    assert_eq!(
        b.get_open_position(&a, "SBIN", Exchange::Nse, Product::Cnc)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        b.get_open_position(&a, "NIFTY30APR26FUT", Exchange::Nfo, Product::Nrml)
            .await
            .unwrap(),
        -75
    );
}

#[tokio::test]
async fn books_funds_and_margin() {
    let (fake, b, _symbols) = broker_with_master().await;
    let a = auth_token();
    let ob = b.get_order_book(&a).await.unwrap();
    assert_eq!(ob.len(), 4);
    assert_eq!(ob[1].symbol, "NIFTY30APR2624000CE");
    assert!(ob.iter().all(|o| o.status == o.status.to_lowercase()));
    // The first trade-book read is throttled and retried.
    let tb = b.get_trade_book(&a).await.unwrap();
    assert_eq!(tb[0].symbol, "SBIN");
    assert_eq!(fake.trades_calls.load(Ordering::SeqCst), 2);
    let pos = b.get_positions(&a).await.unwrap();
    assert_eq!(pos[1].symbol, "NIFTY30APR26FUT");
    let h = b.get_holdings(&a).await.unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].symbol, "SBIN");

    let f = b.get_funds(&a).await.unwrap();
    assert_eq!(f.available_cash, 105000.5);
    assert_eq!(f.utilised_debits, 1500.25);

    let legs = vec![
        MarginLeg {
            key: QuoteKey::new("NFO", "NIFTY30APR2624000CE"),
            action: openalgo_desktop_lib::brokers::common::mapping::Action::Sell,
            quantity: 75,
            product: Product::Nrml,
            pricetype: openalgo_desktop_lib::brokers::common::mapping::PriceType::Market,
            price: 0.0,
            trigger_price: 0.0,
        },
        MarginLeg {
            key: QuoteKey::new("NFO", "UNKNOWN"),
            action: openalgo_desktop_lib::brokers::common::mapping::Action::Buy,
            quantity: 75,
            product: Product::Nrml,
            pricetype: openalgo_desktop_lib::brokers::common::mapping::PriceType::Market,
            price: 0.0,
            trigger_price: 0.0,
        },
    ];
    let m = b.calculate_margin(&a, &legs).await.unwrap();
    assert_eq!(m.total_margin_required, 15000.5);
    assert_eq!(
        fake.calls("POST", "/spanexposure")[0].body,
        json!([{"instrumentId":"40001","exchange":"NSEFO","transactionType":"SELL","quantity":75}])
    );
    assert!(b.calculate_margin(&a, &legs[1..]).await.is_err());
}

#[tokio::test]
async fn quotes_depth_and_history() {
    let (fake, b, _symbols) = broker_with_master().await;
    let a = auth_token();
    let q = b
        .get_quote(&a, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((q.ltp, q.close, q.bid, q.oi), (812.35, 800.0, 812.3, 0));
    assert_eq!(
        fake.calls("POST", "/marketdata/marketquotes")[0].body,
        json!([{"exchange":"NSEEQ","instrumentId":"3045"}])
    );
    assert!(
        fake.calls("POST", "/marketdata/openinterest").is_empty(),
        "no OI for cash"
    );
    // Index quotes go to the stored segment.
    b.get_quote(&a, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(
        fake.calls("POST", "/marketdata/marketquotes")[1].body,
        json!([{"exchange":"NSEEQ","instrumentId":"26000"}])
    );

    let mq = b
        .get_multiquotes(
            &a,
            &[
                QuoteKey::new("NFO", "NIFTY30APR2624000CE"),
                QuoteKey::new("NSE", "NOPE"),
                QuoteKey::new("NSE", "SBIN"),
            ],
        )
        .await
        .unwrap();
    assert_eq!(mq.len(), 3);
    let opt = mq[0].data.as_ref().unwrap();
    assert_eq!((opt.ltp, opt.oi), (120.5, 1_234_500));
    assert!(mq[1].data.is_none() && mq[1].error.as_deref().unwrap().contains("NOPE"));
    assert_eq!(mq[2].data.as_ref().unwrap().ltp, 812.35);
    assert_eq!(
        fake.calls("POST", "/marketdata/openinterest")[0].body,
        json!({"exchange":"NSEFO","instrumentId":"40001"})
    );

    let d = b
        .get_market_depth(&a, &QuoteKey::new("NFO", "NIFTY30APR2624000CE"))
        .await
        .unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.oi, 1_234_500, "OI fetched when the depth row has none");

    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 4, 3).unwrap(),
    };
    let c = b.get_history(&a, &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(
        fake.calls("POST", "/marketdata/historicaldata")[0].body,
        json!({"exchange":"NSEEQ","instrumentId":"3045","interval":"5 minutes",
               "fromDate":"01-Apr-2026","toDate":"03-Apr-2026"})
    );
    let bad = HistoryRequest {
        interval: "2m".into(),
        ..req
    };
    assert!(b.get_history(&a, &bad).await.is_err());
}
