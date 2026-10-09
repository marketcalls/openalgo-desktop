//! Angel One adapter end to end against a local fake SmartAPI (ephemeral
//! port): headers (X-PrivateKey on every call, Bearer JWT), request bodies,
//! and the mapped OpenAlgo answers for auth, orders, books, funds, quotes,
//! depth, history, margin and GTT.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::angel::master_contract::parse_master;
use openalgo_desktop_lib::brokers::angel::AngelBroker;
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../fixtures/brokers/angel/", $name))
    };
}

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    private_key: String,
    authorization: String,
    body: Value,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    quote_calls: AtomicUsize,
    /// How `placeOrder` answers: "" (success), "empty502", "partial",
    /// "cutoff" (the body breaks off mid-reply), "refused". With a non-empty
    /// mode the order book holds the placed
    /// order under its tag when `book_has_order` is set.
    place_mode: Mutex<&'static str>,
    book_has_order: AtomicBool,
}

impl Fake {
    fn bodies(&self, path_suffix: &str) -> Vec<Value> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.path.ends_with(path_suffix))
            .map(|s| s.body.clone())
            .collect()
    }
}

fn ok(v: &str) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        v.to_string(),
    )
        .into_response()
}

fn route(fake: &Fake, path: &str, body: &Value) -> Response {
    let leaf = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    match leaf {
        "loginByPassword" => {
            let errors: Value = serde_json::from_str(fixture!("errors.json")).unwrap();
            if body["totp"] == "000000" {
                ok(&errors["login_failed"].to_string())
            } else {
                ok(&errors["login_ok"].to_string())
            }
        }
        "placeOrder" => match *fake.place_mode.lock() {
            "empty502" => (StatusCode::BAD_GATEWAY, "").into_response(),
            "partial" => ok(r#"{"status":true}"#),
            "cutoff" => {
                let parts: Vec<std::io::Result<Bytes>> = vec![
                    Ok(Bytes::from_static(br#"{"status":true,"da"#)),
                    Err(std::io::Error::other("connection reset")),
                ];
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    axum::body::Body::from_stream(futures_util::stream::iter(parts)),
                )
                    .into_response()
            }
            "refused" => ok(r#"{"status":false,"message":"Order rejected by RMS","errorcode":"AB4008","data":null}"#),
            _ => ok(r#"{"status":true,"message":"SUCCESS","errorcode":"","data":{"script":"SBIN-EQ","orderid":"261003000000099","uniqueorderid":"u"}}"#),
        },
        "getOrderBook" if !fake.place_mode.lock().is_empty() => {
            let placed = fake.bodies("placeOrder");
            let rows: Vec<Value> = if fake.book_has_order.load(Ordering::SeqCst) {
                placed
                    .iter()
                    .map(|p| json!({
                        "orderid": "261003000000777", "ordertag": p["ordertag"],
                        "tradingsymbol": p["tradingsymbol"], "symboltoken": p["symboltoken"],
                        "exchange": p["exchange"], "transactiontype": p["transactiontype"],
                        "quantity": p["quantity"], "status": "open"
                    }))
                    .collect()
            } else {
                vec![json!({"orderid": "older", "ordertag": "openalgo", "tradingsymbol": "SBIN-EQ",
                            "symboltoken": "3045", "exchange": "NSE", "transactiontype": "BUY",
                            "quantity": "10", "status": "open"})]
            };
            ok(&json!({"status": true, "message": "SUCCESS", "errorcode": "", "data": rows}).to_string())
        }
        "modifyOrder" => {
            let errors: Value = serde_json::from_str(fixture!("errors.json")).unwrap();
            ok(&errors["modify_ok"].to_string())
        }
        "cancelOrder" => ok(&json!({"status":true,"message":"SUCCESS","errorcode":"","data":{"orderid": body["orderid"]}}).to_string()),
        "getOrderBook" => ok(fixture!("order_book.json")),
        "getTradeBook" => ok(fixture!("trade_book.json")),
        "getPosition" => ok(fixture!("positions.json")),
        "getAllHolding" => ok(fixture!("holdings.json")),
        "getRMS" => ok(fixture!("rms.json")),
        "quote" => {
            // The first quote call is rate limited, as Angel does with a
            // plain-text 403; the adapter must retry it.
            if fake.quote_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return (
                    StatusCode::FORBIDDEN,
                    "Access denied because of exceeding access rate",
                )
                    .into_response();
            }
            ok(fixture!("quote.json"))
        }
        "getCandleData" => ok(fixture!("candles_minute.json")),
        "getOIData" => ok(fixture!("oi_data.json")),
        "batch" => ok(fixture!("margin_batch.json")),
        "createRule" => {
            let id = if body["triggerprice"] == "880" { "1001" } else { "1002" };
            if body["triggerprice"] == "666" {
                return ok(r#"{"status":false,"message":"Invalid Price","errorcode":"AB9008","data":null}"#);
            }
            ok(&json!({"status":true,"message":"SUCCESS","errorcode":"","data":{"id": id}}).to_string())
        }
        "modifyRule" | "cancelRule" => ok(&json!({"status":true,"message":"SUCCESS","errorcode":"","data":{"id": body["id"]}}).to_string()),
        "ruleDetails" => ok(&json!({"status":true,"message":"SUCCESS","errorcode":"","data":{"id": body["id"], "symboltoken":"3045","exchange":"NSE","status":"NEW"}}).to_string()),
        "ruleList" => ok(fixture!("gtt_rule_list.json")),
        _ => (StatusCode::NOT_FOUND, "no such endpoint").into_response(),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |_m: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let h = |k: &str| {
                    headers
                        .get(k)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string()
                };
                let path = uri.path().to_string();
                fake.seen.lock().push(Seen {
                    path: path.clone(),
                    private_key: h("x-privatekey"),
                    authorization: h("authorization"),
                    body: body.clone(),
                });
                route(&fake, &path, &body)
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
    let r = SymbolResolver::new();
    r.load(parse_master(fixture!("master.json").as_bytes()).unwrap());
    r
}

async fn setup() -> (AngelBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let base = serve(fake.clone()).await;
    let b = AngelBroker::with_base_url(master(), base);
    let auth = AuthToken::new("myapikey:jwt.token.sig")
        .with_feed(Some("feedtok"))
        .with_user_id("<USER_ID>");
    (b, fake, auth)
}

fn order(symbol: &str, exchange: &str, pricetype: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 10,
        price: 950.5,
        order_type: pricetype.into(),
        product: "CNC".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[tokio::test]
async fn login_stores_api_key_and_jwt_and_feed_token() {
    let (b, fake, _) = setup().await;
    let creds = BrokerCredentials {
        api_key: "myapikey".into(),
        client_id: Some("<USER_ID>".into()),
        password: Some("1234".into()),
        totp: Some("123456".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    assert_eq!(r.auth_token, "myapikey:eyJhbGciOiJIUzUxMiJ9.<JWT>.sig");
    assert_eq!(
        r.feed_token.as_deref(),
        Some("eyJhbGciOiJIUzUxMiJ9.<FEED>.sig")
    );
    assert_eq!(r.user_id, "<USER_ID>");
    let seen = fake.seen.lock()[0].clone();
    assert_eq!(seen.private_key, "myapikey");
    assert_eq!(seen.authorization, "");
    assert_eq!(
        seen.body,
        json!({"clientcode":"<USER_ID>","password":"1234","totp":"123456"})
    );
    let bad = BrokerCredentials {
        totp: Some("000000".into()),
        ..creds
    };
    let e = b.authenticate(bad).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert_eq!(e.client_message(), "Invalid totp");
}

#[tokio::test]
async fn place_modify_cancel_send_private_key_and_full_bodies() {
    let (b, fake, auth) = setup().await;
    let r = b
        .place_order(&auth, &order("SBIN", "NSE", "LIMIT"))
        .await
        .unwrap();
    assert_eq!(r.order_id, "261003000000099");
    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "CNC".into(),
        pricetype: "LIMIT".into(),
        quantity: 10,
        price: 951.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("261003000000001", &m, &master()).unwrap();
    let r = b.modify_order(&auth, &rm).await.unwrap();
    assert_eq!(r.order_id, "261003000000001");
    let r = b.cancel_order(&auth, "261003000000001").await.unwrap();
    assert_eq!(r.order_id, "261003000000001");

    let seen = fake.seen.lock().clone();
    assert_eq!(seen.len(), 3);
    for s in &seen {
        assert_eq!(s.private_key, "myapikey", "{}", s.path);
        assert_eq!(s.authorization, "Bearer jwt.token.sig");
    }
    let place = &seen[0].body;
    assert_eq!(place["tradingsymbol"], "SBIN-EQ");
    assert_eq!(place["symboltoken"], "3045");
    assert_eq!(place["triggerprice"], "0");
    assert_eq!(place["producttype"], "DELIVERY");
    let modify = &seen[1].body;
    assert_eq!(modify["tradingsymbol"], "SBIN-EQ");
    assert_eq!(modify["exchange"], "NSE");
    assert_eq!(modify["producttype"], "DELIVERY");
    assert_eq!(modify["variety"], "NORMAL");
    assert_eq!(
        seen[2].body,
        json!({"variety":"NORMAL","orderid":"261003000000001"})
    );
}

#[tokio::test]
async fn books_funds_and_bulk_order_actions() {
    let (b, fake, auth) = setup().await;
    let orders = b.get_order_book(&auth).await.unwrap();
    assert_eq!(orders[0].symbol, "SBIN");
    assert_eq!(orders[1].status, "trigger pending");
    let trades = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(trades[0].average_price, 9008.0);
    let positions = b.get_positions(&auth).await.unwrap();
    assert_eq!(positions[1].symbol, "NIFTY27OCT2625000CE");
    let holdings = b.get_holdings(&auth).await.unwrap();
    assert_eq!(holdings[0].symbol, "SBIN");
    // Angel's own portfolio totals reach /api/v1/holdings statistics.
    let book = b.get_holdings_with_totals(&auth).await.unwrap();
    assert_eq!(book.holdings, holdings);
    let totals = book.totals.unwrap();
    assert_eq!(
        (totals.totalholdingvalue, totals.totalprofitandloss),
        (9541.0, 1241.0)
    );
    assert_eq!(book.statistics().totalpnlpercentage, 14.95);
    let funds = b.get_funds(&auth).await.unwrap();
    assert!((funds.available_cash - 2_043_799.20).abs() < 1e-6);
    assert_eq!(funds.m2m_realized, 12.5);
    assert_eq!(funds.m2m_unrealized, 2706.25);

    // Only the raw `open` and `trigger pending` rows are cancelled.
    let c = b.cancel_all_orders(&auth).await.unwrap();
    assert_eq!(c.cancelled, ["261003000000001", "261003000000002"]);
    assert!(c.failed.is_empty());
    assert_eq!(
        b.get_open_position(&auth, "CRUDEOIL19OCT26FUT", Exchange::Mcx, Product::Mis)
            .await
            .unwrap(),
        100
    );
    assert_eq!(
        b.get_open_position(&auth, "SBIN", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        0
    );
    let closed = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(closed.placed.len(), 2);
    let exits = fake.bodies("placeOrder");
    assert_eq!(exits[0]["tradingsymbol"], "CRUDEOIL19OCT26FUT");
    assert_eq!(exits[0]["transactiontype"], "SELL");
    assert_eq!(exits[0]["producttype"], "INTRADAY");
    assert_eq!(exits[1]["transactiontype"], "BUY");
    assert_eq!(exits[1]["quantity"], "75");
    assert_eq!(exits[1]["producttype"], "CARRYFORWARD");
    assert!(fake.seen.lock().iter().all(|s| s.private_key == "myapikey"));
}

#[tokio::test]
async fn quotes_retry_rate_limit_and_keep_openalgo_names() {
    let (b, fake, auth) = setup().await;
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(
        (q.symbol.as_str(), q.exchange.as_str()),
        ("NIFTY", "NSE_INDEX")
    );
    assert_eq!(q.ltp, 22512.35);
    // First call was refused for rate, the retry succeeded.
    assert_eq!(fake.quote_calls.load(Ordering::SeqCst), 2);
    let body = fake.bodies("quote/").pop().unwrap();
    assert_eq!(
        body,
        json!({"mode":"FULL","exchangeTokens":{"NSE":["99926000"]}})
    );

    // Request order is kept; a repeated key is fetched once.
    let keys: Vec<QuoteKey> = vec![
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NSE", "RELIANCE"),
        QuoteKey::new("NSE", "SBIN"),
    ];
    let res = b.get_multiquotes(&auth, &keys).await.unwrap();
    let batch = fake.bodies("quote/").pop().unwrap();
    assert_eq!(
        batch,
        json!({"mode":"FULL","exchangeTokens":{"NFO":["43210"],"NSE":["2885","3045"]}})
    );
    assert_eq!(res[4].data.as_ref().unwrap().ltp, 954.1);
    assert_eq!(res.len(), keys.len());
    assert_eq!(res[0].data.as_ref().unwrap().bid, 954.05);
    assert_eq!(res[1].data.as_ref().unwrap().oi, 4_567_800);
    assert_eq!(res[2].error.as_deref(), Some("Could not resolve token"));
    assert_eq!(res[3].error.as_deref(), Some("No quote data available"));

    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((d.bids.len(), d.asks.len()), (5, 5));
    assert_eq!(d.total_buy_qty, 512_633);
}

#[tokio::test]
async fn history_fetches_candles_then_oi_for_derivatives() {
    let (b, fake, auth) = setup().await;
    let req = HistoryRequest {
        key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        interval: "1m".into(),
        start: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
    };
    let c = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].timestamp, 1_790_826_300);
    assert_eq!((c[0].oi, c[1].oi), (4_500_000, 4_512_750));
    let body = fake.bodies("getCandleData").pop().unwrap();
    assert_eq!(
        body,
        json!({"exchange":"NFO","symboltoken":"43210","interval":"ONE_MINUTE","fromdate":"2026-10-01 00:00","todate":"2026-10-01 23:59"})
    );
    assert_eq!(fake.bodies("getOIData").len(), 1);

    // An index: exchange mapped to NSE, no OI call; a 45-day 1m range is
    // two chunks (30 days per request).
    let req = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "1m".into(),
        start: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
    };
    b.get_history(&auth, &req).await.unwrap();
    let bodies = fake.bodies("getCandleData");
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[1]["exchange"], "NSE");
    assert_eq!(bodies[1]["todate"], "2026-08-30 23:59");
    assert_eq!(bodies[2]["fromdate"], "2026-08-31 00:00");
    assert_eq!(fake.bodies("getOIData").len(), 1);
    let e = b
        .get_history(
            &auth,
            &HistoryRequest {
                interval: "1w".into(),
                ..req
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn margin_batch_endpoint() {
    let (b, fake, auth) = setup().await;
    let legs = [MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        action: Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    }];
    let r = b.calculate_margin(&auth, &legs).await.unwrap();
    assert_eq!(r.total_margin_required, 191_119.2);
    assert_eq!(r.span_margin, 112_760.0);
    let body = fake.bodies("margin/v1/batch").pop().unwrap();
    assert_eq!(body["positions"][0]["token"], "43210");
    assert_eq!(body["positions"][0]["orderType"], "MARKET");
    let none = [MarginLeg {
        key: QuoteKey::new("NSE", "NOPE"),
        ..legs[0].clone()
    }];
    assert_eq!(
        b.calculate_margin(&auth, &none).await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
}

fn gtt(kind: GttTriggerType) -> GttRequest {
    GttRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        trigger_type: kind,
        action: Action::Buy,
        product: Product::Cnc,
        quantity: 1,
        pricetype: PriceType::Limit,
        price: 901.0,
        trigger_price: 900.5,
        triggerprice_sl: 880.0,
        stoploss: 879.0,
        triggerprice_tg: 990.0,
        target: 991.0,
        last_price: None,
    }
}

#[tokio::test]
async fn gtt_place_modify_cancel_and_book() {
    let (b, fake, auth) = setup().await;
    let r = b.place_gtt(&auth, &gtt(GttTriggerType::Oco)).await.unwrap();
    assert_eq!(r.trigger_id, "1001-1002");
    let r = b
        .modify_gtt(&auth, "1001-1002", &gtt(GttTriggerType::Oco))
        .await
        .unwrap();
    assert_eq!(r.trigger_id, "1001-1002");
    let mods = fake.bodies("modifyRule");
    assert_eq!(
        (mods[0]["id"].as_str(), mods[1]["id"].as_str()),
        (Some("1001"), Some("1002"))
    );
    // A single GTT cannot reuse an OCO trigger id.
    let e = b
        .modify_gtt(&auth, "1001-1002", &gtt(GttTriggerType::Single))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
    let r = b.cancel_gtt(&auth, "1001-1002").await.unwrap();
    assert_eq!(r.trigger_id, "1001-1002");
    let cancels = fake.bodies("cancelRule");
    assert_eq!(
        cancels[0],
        json!({"id":"1001","symboltoken":"3045","exchange":"NSE"})
    );
    // Second OCO leg refused: the first is rolled back.
    let mut bad = gtt(GttTriggerType::Oco);
    bad.triggerprice_tg = 666.0;
    let e = b.place_gtt(&auth, &bad).await.unwrap_err();
    assert_eq!(e.client_message(), "Invalid Price");
    assert_eq!(fake.bodies("cancelRule").len(), 3);
    let book = b.get_gtt_book(&auth, false).await.unwrap();
    assert_eq!(book.len(), 2);
    let list = fake.bodies("ruleList").pop().unwrap();
    assert_eq!(
        list,
        json!({"status":["NEW","ACTIVE","SENTTOEXCHANGE"],"page":1,"count":10})
    );
}

#[tokio::test]
async fn malformed_token_is_refused_before_any_call() {
    let b = AngelBroker::with_base_url(master(), "http://127.0.0.1:9");
    let e = b
        .get_order_book(&AuthToken::new("garbage"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
}

// Ambiguous placement replies are settled from the order book by the order's
// tag and never retried (web #2176).
#[tokio::test]
async fn ambiguous_place_reply_is_reconciled_by_ordertag_without_retry() {
    for mode in ["empty502", "partial", "cutoff"] {
        let (b, fake, auth) = setup().await;
        *fake.place_mode.lock() = mode;
        fake.book_has_order.store(true, Ordering::SeqCst);
        let r = b
            .place_order(&auth, &order("SBIN", "NSE", "LIMIT"))
            .await
            .unwrap();
        assert_eq!(r.order_id, "261003000000777", "{mode}");
        let placed = fake.bodies("placeOrder");
        assert_eq!(placed.len(), 1, "the POST is never retried ({mode})");
        let tag = placed[0]["ordertag"].as_str().unwrap();
        assert!(tag.starts_with("oa") && tag.len() < 20, "{tag}");
        assert_eq!(fake.bodies("getOrderBook").len(), 1);
    }
}

#[tokio::test]
async fn unmatched_ambiguous_reply_is_not_reported_as_success() {
    let (b, fake, auth) = setup().await;
    *fake.place_mode.lock() = "empty502";
    let e = b
        .place_order(&auth, &order("SBIN", "NSE", "LIMIT"))
        .await
        .unwrap_err();
    assert!(
        e.client_message().contains("Check the order book"),
        "{}",
        e.client_message()
    );
    assert_eq!(fake.bodies("placeOrder").len(), 1);
}

#[tokio::test]
async fn explicit_rejection_does_not_query_the_order_book() {
    let (b, fake, auth) = setup().await;
    *fake.place_mode.lock() = "refused";
    let e = b
        .place_order(&auth, &order("SBIN", "NSE", "LIMIT"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Order rejected by RMS"));
    assert!(fake.bodies("getOrderBook").is_empty());
}

/// Web test_angel_ambiguous_order_reconciliation.py::test_transport_error_reconciles_without_retry.
/// The connection drops after the order request is read, before any answer:
/// the order is looked up by its tag, and the POST is sent exactly once.
#[tokio::test]
async fn transport_error_reconciles_without_retry() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let posts: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = posts.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            // Read the head, then the body by Content-Length.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let head_end = loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(i + 4);
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < head_end + len {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
            if head.contains("/placeorder") {
                // Taken by the broker, but the answer never arrives.
                seen.lock().push(body);
                drop(sock);
                continue;
            }
            let rows: Vec<Value> = seen
                .lock()
                .iter()
                .map(|p| {
                    json!({
                        "orderid": "accepted-before-timeout", "ordertag": p["ordertag"],
                        "tradingsymbol": p["tradingsymbol"], "symboltoken": p["symboltoken"],
                        "exchange": p["exchange"], "transactiontype": p["transactiontype"],
                        "quantity": p["quantity"], "status": "open"
                    })
                })
                .collect();
            let reply =
                json!({"status": true, "message": "SUCCESS", "errorcode": "", "data": rows})
                    .to_string();
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        reply.len(),
                        reply
                    )
                    .as_bytes(),
                )
                .await;
        }
    });

    let b = AngelBroker::with_base_url(master(), base);
    let auth = AuthToken::new("myapikey:jwt.token.sig").with_user_id("<USER_ID>");
    let r = b
        .place_order(&auth, &order("SBIN", "NSE", "LIMIT"))
        .await
        .unwrap();
    server.abort();
    assert_eq!(r.order_id, "accepted-before-timeout");
    let posts = posts.lock();
    assert_eq!(posts.len(), 1, "the POST is never retried");
    let tag = posts[0]["ordertag"].as_str().unwrap();
    assert!(tag.starts_with("oa") && tag.len() < 20, "{tag}");
}
