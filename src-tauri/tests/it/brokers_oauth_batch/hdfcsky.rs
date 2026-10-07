//! HDFC Sky against a local fake (REST on axum, the feed on a raw
//! tokio-tungstenite listener).

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use base64::Engine;
use chrono::NaiveDate;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::families::noren::zip;
use openalgo_desktop_lib::brokers::hdfcsky::master_contract::parse_csv;
use openalgo_desktop_lib::brokers::hdfcsky::proto;
use openalgo_desktop_lib::brokers::hdfcsky::HdfcSkyBroker;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use prost::Message as _;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../fixtures/brokers/hdfcsky/", $name))
    };
}

const CLIENT: &str = "TESTCLIENT";

fn jwt() -> String {
    let enc = |v: Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
    };
    format!(
        "{}.{}.signature",
        enc(json!({"alg": "HS256", "typ": "JWT"})),
        enc(json!({"sub": CLIENT, "exp": 1_900_000_000}))
    )
}

#[derive(Debug, Clone)]
struct Seen {
    method: Method,
    path: String,
    query: HashMap<String, String>,
    authorization: Option<String>,
    user_agent: String,
    body: Value,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    /// fetch-ltp answers 429 this many times first.
    ltp_429: AtomicUsize,
    orders: AtomicUsize,
}

impl Fake {
    fn calls(&self, method: Method, path: &str) -> Vec<Seen> {
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

fn status(code: StatusCode, v: Value) -> Response {
    (code, [("content-type", "application/json")], v.to_string()).into_response()
}

fn ltp_row(exchange: &str, token: &str) -> Option<Value> {
    let (ltp, pc) = match (exchange, token) {
        ("NSE", "2885") => (1400.0, 1390.0),
        ("NSE_INDEX", "26000") => (25100.0, 25000.0),
        // The parent cash code silently omits an index.
        ("NSE", "26000") => return None,
        ("NFO", "35001") => (24900.0, 25000.0),
        ("NFO", "40001") => (210.0, 200.0),
        _ => (100.0, 99.0),
    };
    Some(json!({"exchange": exchange, "token": token, "ltp": ltp, "prev_close": pc}))
}

fn route(fake: &Fake, s: &Seen) -> Response {
    let q = |k: &str| s.query.get(k).cloned().unwrap_or_default();
    if s.authorization.as_deref() == Some("expired") {
        return status(
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid credentials"}),
        );
    }
    match (s.method.clone(), s.path.as_str()) {
        (Method::POST, "/oapi/v1/access-token") => {
            if q("request_token") == "bad" {
                status(
                    StatusCode::BAD_REQUEST,
                    json!({"status": "error", "message": "Invalid request token"}),
                )
            } else {
                ok(json!({"accessToken": jwt()}))
            }
        }
        (Method::POST, "/oapi/v1/orders") => {
            if s.body["quantity"] == 999 {
                // HDFC Sky refuses with HTTP 200.
                ok(
                    json!({"status": "error", "message": "algo orders from merchant can only of Limit type"}),
                )
            } else {
                let n = fake.orders.fetch_add(1, Ordering::SeqCst) + 1;
                ok(json!({"status": "success", "data": {"oms_order_id": format!("OID{}", n)}}))
            }
        }
        (Method::PUT, "/oapi/v1/orders") => ok(json!({
            "status": "success",
            "data": {"oms_order_id": s.body["oms_order_id"]}
        })),
        (Method::DELETE, p) if p.starts_with("/oapi/v1/orders/") => {
            if p.ends_with("260101000002") {
                ok(json!({"status": "error", "message": "Order already traded"}))
            } else {
                ok(json!({"status": "success", "data": {}}))
            }
        }
        (Method::GET, "/oapi/v1/orders") => match q("type").as_str() {
            "pending" => ok(fixture!("orders_pending.json")),
            "completed" => ok(fixture!("orders_completed.json")),
            _ => status(StatusCode::BAD_REQUEST, json!({"status": "error"})),
        },
        (Method::GET, "/oapi/v1/trades") => ok(fixture!("trades.json")),
        (Method::GET, "/oapi/v1/positions") => ok(fixture!("positions.json")),
        (Method::GET, "/oapi/v1/holdings") => ok(fixture!("holdings.json")),
        (Method::GET, "/oapi/v1/funds/view") => ok(fixture!("funds.json")),
        (Method::POST, "/oapi/v1/margin") => ok(fixture!("margin.json")),
        (Method::PUT, "/oapi/v1/fetch-ltp") => {
            if fake
                .ltp_429
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return status(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({"error": "merchantKeyRateLimit - too many requests"}),
                );
            }
            let items = s.body["data"].as_array().cloned().unwrap_or_default();
            if items.len() > 10 {
                return status(
                    StatusCode::BAD_REQUEST,
                    json!({"status": "error", "message": "maximum 10 items allowed"}),
                );
            }
            let rows: Vec<Value> = items
                .iter()
                .filter_map(|i| {
                    ltp_row(
                        i["exchange"].as_str().unwrap_or(""),
                        i["token"].as_str().unwrap_or(""),
                    )
                })
                .collect();
            ok(json!({"status": "success", "data": rows}))
        }
        (Method::GET, "/oapi/charts-api/charts/v1/fetch-candle") => {
            let empty = json!({"meta": {"err_code": "SUCCESS"}, "data": {"results": []}});
            match q("symbol").as_str() {
                "ZZTEST26XYZFUT" => status(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"meta": {"displayMessage": "Chart service unavailable"}}),
                ),
                // Indices answer to the broker name, not the OpenAlgo one.
                "NIFTY" => ok(empty),
                _ if q("chartType") == "DAY" => ok(fixture!("candles_day.json")),
                _ => {
                    let has =
                        q("start").as_str() <= "2026-10-05" && q("end").as_str() >= "2026-10-05";
                    if has {
                        ok(fixture!("candles_minute.json"))
                    } else {
                        ok(empty)
                    }
                }
            }
        }
        (Method::GET, "/master.zip") => (
            StatusCode::OK,
            zip::build(
                "CompactScrip.csv",
                fixture!("CompactScrip.csv").as_bytes(),
                true,
            ),
        )
            .into_response(),
        _ => status(StatusCode::NOT_FOUND, json!({"error": "no such endpoint"})),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let query: HashMap<String, String> = uri
                    .query()
                    .map(|q| serde_urlencoded::from_str(q).unwrap_or_default())
                    .unwrap_or_default();
                let h = |k: &str| {
                    headers
                        .get(k)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                };
                let seen = Seen {
                    method,
                    path: uri.path().to_string(),
                    query,
                    authorization: h("authorization"),
                    user_agent: h("user-agent").unwrap_or_default(),
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

fn mbp_frame(token: i64) -> Vec<u8> {
    let level = |q: i64, p: f64, buy: bool| proto::MarketDepthDto {
        quantity: q,
        price: p,
        number_of_orders: 3,
        buy_flag: buy,
    };
    proto::GenericDtoList {
        generic_dto_list: vec![proto::GenericDto {
            instrument_id: token,
            packet_type: proto::packet_type::NSE_CM_ALL,
            mbp_data: Some(proto::MbpData {
                last_traded_price: 1400.0,
                last_trade_quantity: 12,
                total_buy_quantity: 40_000,
                total_sell_quantity: 50_000,
                market_depth_dto_list: Some(proto::MarketDepthDtoList {
                    market_depth_dto: vec![
                        level(100, 1399.9, true),
                        level(150, 1399.8, true),
                        level(200, 1400.1, false),
                    ],
                }),
                ..Default::default()
            }),
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// A one-connection feed: answers the first subscribe with one MBP packet
/// per subscribed scrip and records what it received.
async fn serve_feed(seen: Arc<Mutex<Vec<String>>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let path = Arc::new(Mutex::new(String::new()));
                let p2 = path.clone();
                use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
                let cb = move |req: &Request, resp: Response| {
                    *p2.lock() = req.uri().to_string();
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, cb).await else {
                    return;
                };
                seen.lock().push(path.lock().clone());
                while let Some(Ok(msg)) = ws.next().await {
                    if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
                        seen.lock().push(t.to_string());
                        let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                        for s in v["subscribe"].as_array().cloned().unwrap_or_default() {
                            let id = s["scripId"].as_str().unwrap_or("");
                            let tok: i64 =
                                id.rsplit('_').next().unwrap_or("0").parse().unwrap_or(0);
                            let _ = ws
                                .send(tokio_tungstenite::tungstenite::Message::Binary(mbp_frame(
                                    tok,
                                )))
                                .await;
                        }
                    }
                }
            });
        }
    });
    format!("ws://{}/wsapi/v1/session", addr)
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_csv(fixture!("CompactScrip.csv")).unwrap());
    r
}

async fn setup_with_ws(ws: &str) -> (HdfcSkyBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let b = HdfcSkyBroker::with_base_url(master(), &host, format!("{}/master.zip", host), ws);
    (b, fake, AuthToken::new(format!("APPKEY:{}", jwt())))
}

async fn setup() -> (HdfcSkyBroker, Arc<Fake>, AuthToken) {
    // Nothing listens on port 1: feed snapshots fail fast and REST data flows.
    setup_with_ws("ws://127.0.0.1:1/wsapi/v1/session").await
}

fn order(symbol: &str, exchange: &str, side: &str, pricetype: &str, qty: i32) -> OrderRequest {
    OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: side.into(),
        quantity: qty,
        price: 0.0,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    }
}

#[tokio::test]
async fn hdfcsky_login_exchanges_the_request_token() {
    let (b, fake, _) = setup().await;
    let r = b
        .authenticate(BrokerCredentials {
            api_key: "APPKEY".into(),
            api_secret: Some("SECRET".into()),
            request_token: Some("rt-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.auth_token, format!("APPKEY:{}", jwt()));
    assert_eq!(r.user_id, CLIENT);
    let call = &fake.calls(Method::POST, "/oapi/v1/access-token")[0];
    assert_eq!(call.query["api_key"], "APPKEY");
    assert_eq!(call.query["request_token"], "rt-1");
    assert_eq!(call.body, json!({"apiSecret": "SECRET"}));
    assert!(call.authorization.is_none());
    assert!(call.user_agent.starts_with("Mozilla/5.0"));

    let e = b
        .authenticate(BrokerCredentials {
            api_key: "APPKEY".into(),
            api_secret: Some("SECRET".into()),
            request_token: Some("bad".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert!(e.client_message().contains("Invalid request token"));
    let e = b
        .authenticate(BrokerCredentials {
            api_key: "APPKEY".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn hdfcsky_market_orders_are_sent_as_protected_limits() {
    let (b, fake, auth) = setup().await;
    let syms = master();
    let mkt =
        ResolvedOrder::resolve(&order("RELIANCE", "NSE", "BUY", "MARKET", 10), &syms).unwrap();
    let r = b.place_order(&auth, &mkt).await.unwrap();
    assert_eq!(r.order_id, "OID1");
    let ltp = &fake.calls(Method::PUT, "/oapi/v1/fetch-ltp")[0];
    assert_eq!(
        ltp.body,
        json!({"data": [{"exchange": "NSE", "token": "2885"}]})
    );
    assert!(!ltp.query.contains_key("client_id"));
    let call = &fake.calls(Method::POST, "/oapi/v1/orders")[0];
    assert_eq!(call.authorization.as_deref(), Some(jwt().as_str()));
    assert!(call.user_agent.starts_with("Mozilla/5.0"));
    assert_eq!(call.query["api_key"], "APPKEY");
    assert!(!call.query.contains_key("client_id"));
    let body = &call.body;
    assert_eq!(body["order_type"], "LIMIT");
    // 1400 + 1%: like the web's `calculate_protected_price(symbol=...)`,
    // RELIANCE ends in "CE" and takes the option slab.
    assert_eq!(body["price"], 1414.0);
    assert_eq!(body["instrument_token"], "2885");
    assert_eq!(body["client_id"], CLIENT);
    assert_eq!(body["order_side"], "BUY");
    assert_eq!(body["product"], "MIS");
    assert_eq!(body["exchange"], "NSE");
    assert!(body["user_order_id"].as_i64().unwrap() < 1_000_000_000);

    // SL-M -> SL priced off the trigger (SELL: below it), no LTP call.
    let mut slm = order("NIFTY27OCT26FUT", "NFO", "SELL", "SL-M", 65);
    slm.trigger_price = Some(24800.0);
    slm.product = "NRML".into();
    let slm = ResolvedOrder::resolve(&slm, &syms).unwrap();
    b.place_order(&auth, &slm).await.unwrap();
    assert_eq!(fake.calls(Method::PUT, "/oapi/v1/fetch-ltp").len(), 1);
    let body = &fake.calls(Method::POST, "/oapi/v1/orders")[1].body;
    assert_eq!(body["order_type"], "SL");
    assert_eq!(body["price"], 24676.0);
    assert_eq!(body["trigger_price"], 24800.0);

    // LIMIT goes out untouched.
    let mut lim = order("RELIANCE", "NSE", "SELL", "LIMIT", 1);
    lim.price = 1500.25;
    let lim = ResolvedOrder::resolve(&lim, &syms).unwrap();
    b.place_order(&auth, &lim).await.unwrap();
    let body = &fake.calls(Method::POST, "/oapi/v1/orders")[2].body;
    assert_eq!(
        (body["order_type"].clone(), body["price"].clone()),
        (json!("LIMIT"), json!(1500.25))
    );

    // An error envelope on HTTP 200 is a refusal.
    let mut bad = order("RELIANCE", "NSE", "BUY", "LIMIT", 999);
    bad.price = 1400.0;
    let bad = ResolvedOrder::resolve(&bad, &syms).unwrap();
    let e = b.place_order(&auth, &bad).await.unwrap_err();
    assert_eq!(e.code(), "BROKER_ERROR");
    assert!(e.client_message().contains("only of Limit type"));
}

#[tokio::test]
async fn hdfcsky_modify_cancel_and_cancel_all() {
    let (b, fake, auth) = setup().await;
    let m = ResolvedModify::resolve(
        "260101000001",
        &ModifyOrderRequest {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "CNC".into(),
            pricetype: "LIMIT".into(),
            quantity: 10,
            price: 1395.5,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    let r = b.modify_order(&auth, &m).await.unwrap();
    assert_eq!(r.order_id, "260101000001");
    let call = &fake.calls(Method::PUT, "/oapi/v1/orders")[0];
    assert_eq!(call.body["oms_order_id"], "260101000001");
    assert_eq!(call.body["price"], 1395.5);
    assert!(!call.query.contains_key("client_id"));

    let r = b.cancel_order(&auth, "OIDX").await.unwrap();
    assert_eq!(r.order_id, "OIDX");
    let call = &fake.calls(Method::DELETE, "/oapi/v1/orders/OIDX")[0];
    assert_eq!(call.query["execution_type"], "REGULAR");
    assert_eq!(call.query["client_id"], CLIENT);
    assert_eq!(call.query["api_key"], "APPKEY");

    let res = b.cancel_all_orders(&auth).await.unwrap();
    assert_eq!(res.cancelled, ["260101000001", "260101000003"]);
    assert_eq!(res.failed, ["260101000002"]);
    // Only the pending half is read; "OPEN" is not broker vocabulary.
    let reads = fake.calls(Method::GET, "/oapi/v1/orders");
    assert!(reads.iter().all(|c| c.query["type"] == "pending"));
}

#[tokio::test]
async fn hdfcsky_books_funds_and_positions() {
    let (b, fake, auth) = setup().await;
    let orders = b.get_order_book(&auth).await.unwrap();
    assert_eq!(orders.len(), 7);
    assert_eq!(orders[0].symbol, "RELIANCE");
    assert_eq!(orders[1].status, "trigger pending");
    assert_eq!(orders[4].status, "complete");
    assert_eq!(orders[5].exchange, "CDS");
    let types: Vec<String> = fake
        .calls(Method::GET, "/oapi/v1/orders")
        .iter()
        .map(|c| c.query["type"].clone())
        .collect();
    assert_eq!(types, ["pending", "completed"]);
    assert!(fake
        .calls(Method::GET, "/oapi/v1/orders")
        .iter()
        .all(|c| c.query["client_id"] == CLIENT));

    let trades = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(trades[1].symbol, "NIFTY27OCT26FUT");
    let pos = b.get_positions(&auth).await.unwrap();
    assert_eq!(pos[0].quantity, -65);
    assert_eq!(
        fake.calls(Method::GET, "/oapi/v1/positions")[0].query["type"],
        "historical"
    );
    let h = b.get_holdings(&auth).await.unwrap();
    assert_eq!(h[0].symbol, "RELIANCE");

    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!(f.available_cash, 100000.0);
    assert_eq!(f.collateral, 5000.0);
    assert_eq!(f.m2m_realized, 125.5);
    let fc = &fake.calls(Method::GET, "/oapi/v1/funds/view")[0];
    assert_eq!(fc.query["type"], "all");
    assert_eq!(fc.query["client_id"], CLIENT);

    assert_eq!(
        b.get_open_position(&auth, "NIFTY27OCT26FUT", Exchange::Nfo, Product::Nrml)
            .await
            .unwrap(),
        -65
    );
    assert_eq!(
        b.get_open_position(&auth, "RELIANCE", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        10
    );
    assert_eq!(
        b.get_open_position(&auth, "RELIANCE", Exchange::Nse, Product::Cnc)
            .await
            .unwrap(),
        0
    );

    let expired = AuthToken::new("APPKEY:expired");
    let e = b.get_funds(&expired).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert!(e.client_message().contains("Log in to HDFC Sky again"));
}

#[tokio::test]
async fn hdfcsky_close_all_squares_off_open_rows() {
    let (b, fake, auth) = setup().await;
    let r = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(r.placed, ["OID1", "OID2"]);
    assert!(r.failed.is_empty());
    let placed = fake.calls(Method::POST, "/oapi/v1/orders");
    assert_eq!(placed.len(), 2);
    let fut = &placed[0].body;
    assert_eq!(fut["order_side"], "BUY");
    assert_eq!(fut["quantity"], 65);
    assert_eq!(fut["product"], "NRML");
    assert_eq!(fut["instrument_token"], "35001");
    assert_eq!(fut["order_type"], "LIMIT");
    // 24900 + 0.5% on the 0.1 tick.
    assert_eq!(fut["price"], 25024.5);
    let eq = &placed[1].body;
    assert_eq!(
        (eq["order_side"].clone(), eq["quantity"].clone()),
        (json!("SELL"), json!(10))
    );
}

#[tokio::test]
async fn hdfcsky_margin_prices_the_underlying() {
    let (b, fake, auth) = setup().await;
    let leg = MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        action: Action::Sell,
        quantity: 65,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price: 210.0,
        trigger_price: 0.0,
    };
    let m = b.calculate_margin(&auth, &[leg]).await.unwrap();
    assert_eq!(m.total_margin_required, 114000.5);
    assert_eq!(m.span_margin, 95000.5);
    let ltp = &fake.calls(Method::PUT, "/oapi/v1/fetch-ltp")[0];
    assert_eq!(
        ltp.body,
        json!({"data": [{"exchange": "NSE_INDEX", "token": "26000"}]})
    );
    let call = &fake.calls(Method::POST, "/oapi/v1/margin")[0];
    let l = &call.body["data"][0];
    assert_eq!(l["underlying"], 25100);
    assert_eq!(l["series"], "OPTIDX");
    assert_eq!(l["symbol"], "NIFTY26OCT25000CE");
    assert_eq!(l["product"], "0");

    let unknown = MarginLeg {
        key: QuoteKey::new("NSE", "NOPE"),
        action: Action::Buy,
        quantity: 1,
        product: Product::Mis,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    };
    let e = b.calculate_margin(&auth, &[unknown]).await.unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn hdfcsky_quotes_and_multiquotes() {
    let (b, fake, auth) = setup().await;
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    assert_eq!((q.ltp, q.close), (1400.0, 1390.0));
    // Session OHLCV from the latest DAY candle.
    assert_eq!(
        (q.open, q.high, q.low, q.volume),
        (1390.0, 1405.0, 1385.0, 5_000_000)
    );
    assert_eq!(q.change, 10.0);
    let chart = &fake.calls(Method::GET, "/oapi/charts-api/charts/v1/fetch-candle")[0];
    assert_eq!(chart.query["chartType"], "DAY");
    assert_eq!(chart.query["seriesType"], "EQ");
    assert_eq!(chart.query["symbol"], "RELIANCE");

    // Index LTP is addressed by NSE_INDEX, not the parent code.
    let n = b
        .get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(n.ltp, 25100.0);

    fake.ltp_429.store(1, Ordering::SeqCst);
    let before = fake.calls(Method::PUT, "/oapi/v1/fetch-ltp").len();
    let mut keys: Vec<QuoteKey> = vec![
        QuoteKey::new("NSE", "RELIANCE"),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
        QuoteKey::new("NSE_INDEX", "NIFTY"),
        QuoteKey::new("NSE_INDEX", "BANKNIFTY"),
        QuoteKey::new("NSE_INDEX", "INDIAVIX"),
        QuoteKey::new("NSE_INDEX", "NIFTYAUTO"),
        QuoteKey::new("BSE_INDEX", "SENSEX"),
        QuoteKey::new("BSE_INDEX", "SENSEX50"),
        QuoteKey::new("BSE", "RELIANCE"),
        QuoteKey::new("NSE", "BAJAJ-AUTO"),
        QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
    ];
    keys.push(QuoteKey::new("CDS", "EURINR01OCT26FUT"));
    let res = b.get_multiquotes(&auth, &keys).await.unwrap();
    assert_eq!(res.len(), keys.len());
    assert_eq!(res[0].data.as_ref().unwrap().ltp, 1400.0);
    assert!(res[1].error.as_deref().unwrap().contains("NSE:NOPE"));
    assert_eq!(res[2].data.as_ref().unwrap().ltp, 24900.0);
    assert_eq!(res[11].data.as_ref().unwrap().close, 200.0);
    // 12 resolvable instruments: one 429, then batches of 10 and 2.
    let ltp_calls = &fake.calls(Method::PUT, "/oapi/v1/fetch-ltp")[before..];
    assert_eq!(ltp_calls.len(), 3);
    let sizes: Vec<usize> = ltp_calls
        .iter()
        .map(|c| c.body["data"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, [10, 10, 2]);
}

#[tokio::test]
async fn hdfcsky_depth_reads_the_book_from_a_feed_snapshot() {
    let feed_seen = Arc::new(Mutex::new(Vec::new()));
    let ws = serve_feed(feed_seen.clone()).await;
    let (b, _fake, auth) = setup_with_ws(&ws).await;
    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    assert_eq!(d.ltp, 1400.0);
    assert_eq!(d.prev_close, 1390.0);
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks.len(), 5);
    assert_eq!((d.bids[0].price, d.bids[0].quantity), (1399.9, 100));
    assert_eq!(d.bids[2].price, 0.0);
    assert_eq!(d.asks[0].price, 1400.1);
    assert_eq!(
        (d.ltq, d.total_buy_qty, d.total_sell_qty),
        (12, 40_000, 50_000)
    );
    let seen = feed_seen.lock().clone();
    assert!(seen[0].contains("token=") && seen[0].contains("api_key=APPKEY"));
    let sub: Value = serde_json::from_str(&seen[1]).unwrap();
    assert_eq!(
        sub,
        json!({"heart_beat": false, "subscribe": [{"scripId": "NSE_2885", "type": "ALL"}]})
    );
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[tokio::test]
async fn hdfcsky_history_chunks_and_resamples() {
    let (b, fake, auth) = setup().await;
    let c = b
        .get_history(
            &auth,
            &HistoryRequest {
                key: QuoteKey::new("NSE", "RELIANCE"),
                interval: "5m".into(),
                start: d(2026, 9, 1),
                end: d(2026, 10, 31),
            },
        )
        .await
        .unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].volume, 9600);
    let calls = fake.calls(Method::GET, "/oapi/charts-api/charts/v1/fetch-candle");
    // MINUTE requests are capped at 31 days.
    let ranges: Vec<(String, String)> = calls
        .iter()
        .map(|c| (c.query["start"].clone(), c.query["end"].clone()))
        .collect();
    assert_eq!(
        ranges,
        [
            ("2026-09-01".to_string(), "2026-10-01".to_string()),
            ("2026-10-02".to_string(), "2026-10-31".to_string())
        ]
    );
    assert!(calls.iter().all(|c| c.query["chartType"] == "MINUTE"));
    assert!(calls.iter().all(|c| !c.query.contains_key("client_id")));

    // Index: the OpenAlgo form returns nothing, the broker form candles;
    // the winning form is remembered for the next request.
    let req = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "W".into(),
        start: d(2026, 9, 1),
        end: d(2026, 10, 31),
    };
    let w = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(w.len(), 2);
    let before = fake
        .calls(Method::GET, "/oapi/charts-api/charts/v1/fetch-candle")
        .len();
    b.get_history(&auth, &req).await.unwrap();
    let after = fake.calls(Method::GET, "/oapi/charts-api/charts/v1/fetch-candle");
    assert_eq!(after.len(), before + 1);
    assert_eq!(after.last().unwrap().query["symbol"], "NIFTY 50");
    assert_eq!(after.last().unwrap().query["seriesType"], "INDICES");
    assert_eq!(after.last().unwrap().query["exchange"], "NSE");

    // Every chunk failing is an error, after the chunk retries.
    let e = b
        .get_history(
            &auth,
            &HistoryRequest {
                key: QuoteKey::new("NFO", "ZZTEST27OCT26FUT"),
                interval: "D".into(),
                start: d(2026, 10, 1),
                end: d(2026, 10, 2),
            },
        )
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Chart service unavailable"));
    let zz = fake
        .calls(Method::GET, "/oapi/charts-api/charts/v1/fetch-candle")
        .into_iter()
        .filter(|c| c.query["symbol"] == "ZZTEST26XYZFUT")
        .count();
    assert_eq!(zz, 4);

    let e = b
        .get_history(
            &auth,
            &HistoryRequest {
                key: QuoteKey::new("NSE", "RELIANCE"),
                interval: "2h".into(),
                start: d(2026, 10, 1),
                end: d(2026, 10, 2),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn hdfcsky_master_contract_downloads_the_zip() {
    let (b, fake, auth) = setup().await;
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert_eq!(rows.len(), 18);
    let call = &fake.calls(Method::GET, "/master.zip")[0];
    assert!(call.authorization.is_none());
    assert!(call.user_agent.starts_with("Mozilla/5.0"));
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTY" && r.exchange == "NSE_INDEX"));
}
