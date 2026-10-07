//! Motilal Oswal against a local fake broker: login with and without the
//! access-token step, the unverified-token refusal, price-protected
//! orders, modify with `lastmodifiedtime`, cancel and cancel-all,
//! close-all, books, funds, quotes (dealer and index-field retries),
//! multiquotes and depth over a fake binary broadcast socket, today's daily
//! bar, and the master download.

#![allow(unused_imports)]

use super::support::*;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::motilal::{FeedTimings, MotilalBroker};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const SESSION: &str = "AT1:::ACC1:::<USER_ID>:::KEY1:::SEC1";

fn auth() -> AuthToken {
    AuthToken::new(SESSION)
}

fn sym(
    symbol: &str,
    exchange: &str,
    brexchange: &str,
    token: &str,
    lot: i32,
    tick: f64,
) -> SymToken {
    row(symbol, symbol, exchange, brexchange, token, lot, tick)
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![
        sym("RELIANCE", "NSE", "NSE", "2885", 1, 0.1),
        sym("INFY", "NSE", "NSE", "1594", 1, 0.05),
        sym("RELAXO", "NSE", "NSE", "10838", 1, 0.05),
        sym("TATASTEEL", "BSE", "BSE", "500470", 1, 0.05),
        sym("NIFTY28OCT26FUT", "NFO", "NSEFO", "35001", 75, 0.1),
        sym("CRUDEOIL19OCT26FUT", "MCX", "MCX", "239484", 100, 1.0),
        sym("NIFTY", "NSE_INDEX", "NSE", "26000", 1, 0.05),
        sym("SENSEX", "BSE_INDEX", "BSE", "999901", 1, 0.05),
        sym("GOLDIDX", "MCX_INDEX", "MCX", "777", 1, 0.05),
    ]);
    r
}

fn fast() -> FeedTimings {
    FeedTimings {
        connect: Duration::from_secs(5),
        depth_wait: Duration::from_millis(1500),
        index_wait: Duration::from_millis(300),
        multi_per_symbol: Duration::from_millis(10),
        multi_min: Duration::from_millis(1500),
        multi_max: Duration::from_millis(2000),
        batch_pause: Duration::from_millis(1),
    }
}

fn broker(base: &str, ws: &str) -> MotilalBroker {
    MotilalBroker::with_urls(master(), base, ws, "ws://127.0.0.1:9/ws").with_timings(fast())
}

fn ltp(paise: i64) -> Value {
    json!({"status":"SUCCESS","message":"","errorcode":"","data":{
        "ltp": paise, "open": paise, "high": paise, "low": paise, "close": paise,
        "bid": 0, "ask": 0, "volume": 10}})
}

fn success() -> Value {
    json!({"status":"SUCCESS","message":"ok","errorcode":""})
}

/// Fake REST broker answering every documented path.
fn rest(req: &Req) -> Response {
    let p = req.path.as_str();
    if p.ends_with("/getltpdata") {
        // NIFTY future at 100.00, everything else 2400.00 (paise).
        let price = if req.json()["scripcode"] == 35001 {
            10000
        } else {
            240000
        };
        return ok(ltp(price));
    }
    if p.ends_with("/placeorder") {
        return ok(
            json!({"status":"SUCCESS","message":"Order placed","errorcode":"","uniqueorderid":"1300000000000777"}),
        );
    }
    if p.ends_with("/getorderbook") {
        return ok(crate::fixture!("motilal", "order_book.json"));
    }
    if p.ends_with("/gettradebook") {
        return ok(crate::fixture!("motilal", "trade_book.json"));
    }
    if p.ends_with("/getposition") {
        return ok(crate::fixture!("motilal", "positions.json"));
    }
    if p.ends_with("/getdpholding") {
        return ok(crate::fixture!("motilal", "holdings.json"));
    }
    if p.ends_with("/getreportmargindetail") {
        return ok(crate::fixture!("motilal", "margin_detail.json"));
    }
    if p.ends_with("/cancelorder") {
        if req.json()["uniqueorderid"] == "1300000000000002" {
            return ok(
                json!({"status":"FAILURE","message":"Order already traded","errorcode":"MO4001"}),
            );
        }
        return ok(success());
    }
    if p.ends_with("/getorderdetailbyuniqueorderid") {
        if req.json()["uniqueorderid"] == "1300000000000001" {
            return ok(
                json!({"status":"SUCCESS","data":[{"uniqueorderid":"1300000000000001",
                "lastmodifiedtime":"0","recordinserttime":"03-Oct-2026 09:20:00","qtytradedtoday":3}]}),
            );
        }
        return ok(json!({"status":"FAILURE","message":"No data","errorcode":"MO1005"}));
    }
    if p.ends_with("/modifyorder") {
        return ok(success());
    }
    with_status(StatusCode::NOT_FOUND, json!({}))
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

fn creds(secret: Option<&str>, totp: &str) -> BrokerCredentials {
    BrokerCredentials {
        api_key: "KEY1".into(),
        api_secret: secret.map(str::to_string),
        client_id: Some("<USER_ID>".into()),
        password: Some("Secret@1".into()),
        totp: Some(totp.into()),
        auth_code: Some("18/10/1988".into()),
        ..Default::default()
    }
}

fn login_fake(req: &Req) -> Response {
    if req.path.ends_with("/authdirectapi") {
        let b = req.json();
        if b["totp"] == "000000" {
            return ok(json!({"status":"FAILURE","message":"Invalid TOTP","errorcode":"MO1093"}));
        }
        if b["totp"] == "111111" {
            return ok(
                json!({"status":"SUCCESS","AuthToken":"AT-1","isAuthTokenVerified":"FALSE"}),
            );
        }
        return ok(
            json!({"status":"SUCCESS","message":"Login Successful","AuthToken":"AT-1","isAuthTokenVerified":"TRUE"}),
        );
    }
    if req.path.ends_with("/getaccesstoken") {
        return ok(json!({"status":"SUCCESS","accesstoken":"ACC-1"}));
    }
    with_status(StatusCode::NOT_FOUND, json!({}))
}

#[tokio::test]
async fn login_with_secret_runs_the_access_token_step() {
    let fake = Fake::start(login_fake).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let r = b.authenticate(creds(Some("SEC1"), "123456")).await.unwrap();
    assert_eq!(r.auth_token, "AT-1:::ACC-1:::<USER_ID>:::KEY1:::SEC1");
    assert_eq!(r.user_id, "<USER_ID>");
    assert!(r.feed_token.is_none());

    let login = &fake.calls("/rest/login/v7/authdirectapi")[0];
    assert_eq!(login.header("ApiKey"), "KEY1");
    assert_eq!(login.header("apisecretkey"), "SEC1");
    assert_eq!(login.header("vendorinfo"), "<USER_ID>");
    assert_eq!(login.header("SourceId"), "WEB");
    assert_eq!(login.header("browsername"), "Chrome");
    assert_eq!(login.header("User-Agent"), "MOSL/V.1.1.0");
    assert!(login.header("Authorization").is_empty());
    let body = login.json();
    assert_eq!(body["2FA"], "18/10/1988");
    assert_eq!(body["totp"], "123456");
    // sha256("Secret@1" + "KEY1"), never the plain password.
    assert_eq!(body["password"].as_str().unwrap().len(), 64);
    assert!(!login.body.contains("Secret@1"));

    let acc = &fake.calls("/rest/login/v1/getaccesstoken")[0];
    assert_eq!(acc.header("Authorization"), "AT-1");
    assert_eq!(acc.header("apisecretkey"), "SEC1");
}

#[tokio::test]
async fn login_without_secret_skips_the_access_token() {
    let fake = Fake::start(login_fake).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let r = b.authenticate(creds(None, "")).await.unwrap();
    assert_eq!(r.auth_token, "AT-1::::::<USER_ID>:::KEY1:::");
    assert!(fake.calls("/getaccesstoken").is_empty());
    let body = fake.calls("/authdirectapi")[0].json();
    assert!(body.get("totp").is_none());
    assert!(fake.calls("/authdirectapi")[0]
        .header("apisecretkey")
        .is_empty());
}

#[tokio::test]
async fn login_refusals_are_trader_messages() {
    let fake = Fake::start(login_fake).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let e = b.authenticate(creds(None, "000000")).await.unwrap_err();
    assert!(
        e.client_message().contains("Invalid TOTP"),
        "{}",
        e.client_message()
    );
    assert!(e.client_message().contains("fresh code"));
    let e = b.authenticate(creds(None, "111111")).await.unwrap_err();
    assert!(e.client_message().contains("authenticator app"));
    let mut c = creds(None, "");
    c.auth_code = None;
    let e = b.authenticate(c).await.unwrap_err();
    assert!(e.client_message().contains("date of birth"));
    let mut c = creds(None, "");
    c.password = None;
    assert!(b.authenticate(c).await.is_err());
    // Only the TOTP-refused and unverified attempts reached Motilal.
    assert_eq!(fake.calls("/authdirectapi").len(), 2);
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

fn order(
    symbol: &str,
    exchange: &str,
    side: &str,
    qty: i32,
    pricetype: &str,
    product: &str,
) -> ResolvedOrder {
    ResolvedOrder::resolve(
        &OrderRequest {
            symbol: symbol.into(),
            exchange: exchange.into(),
            side: side.into(),
            quantity: qty,
            price: 0.0,
            order_type: pricetype.into(),
            product: product.into(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        },
        &master(),
    )
    .unwrap()
}

#[tokio::test]
async fn market_orders_are_sent_as_protected_limits() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let r = b
        .place_order(
            &auth(),
            &order("RELIANCE", "NSE", "BUY", 10, "MARKET", "CNC"),
        )
        .await
        .unwrap();
    assert_eq!(r.order_id, "1300000000000777");
    let q = &fake.calls("/getltpdata")[0];
    assert_eq!(q.json(), json!({"exchange":"NSE","scripcode":2885}));
    let p = &fake.calls("/placeorder")[0];
    assert_eq!(p.header("Authorization"), "AT1");
    assert_eq!(p.header("accesstoken"), "ACC1");
    assert_eq!(p.header("ApiKey"), "KEY1");
    assert_eq!(p.header("vendorinfo"), "<USER_ID>");
    let body = p.json();
    assert_eq!(body["ordertype"], "LIMIT");
    assert_eq!(body["price"], 2424.0);
    assert_eq!(body["producttype"], "DELIVERY");
    assert_eq!(body["quantityinlot"], 10);
    assert_eq!(body["symboltoken"], 2885);

    // Derivatives: quantity in lots, every product NORMAL; a quantity that
    // is not a lot multiple never reaches Motilal.
    b.place_order(
        &auth(),
        &order("NIFTY28OCT26FUT", "NFO", "SELL", 150, "LIMIT", "MIS"),
    )
    .await
    .unwrap();
    let body = fake.calls("/placeorder")[1].json();
    assert_eq!(
        (body["quantityinlot"].as_i64(), body["producttype"].as_str()),
        (Some(2), Some("NORMAL"))
    );
    assert_eq!(body["exchange"], "NSEFO");
    let e = b
        .place_order(
            &auth(),
            &order("NIFTY28OCT26FUT", "NFO", "SELL", 100, "LIMIT", "NRML"),
        )
        .await
        .unwrap_err();
    assert!(e.client_message().contains("lot size"));
    assert_eq!(fake.calls("/placeorder").len(), 2);
}

#[tokio::test]
async fn modify_reads_lastmodifiedtime_with_fallbacks() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let m = ResolvedModify::resolve(
        "1300000000000001",
        &ModifyOrderRequest {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "CNC".into(),
            pricetype: "LIMIT".into(),
            quantity: 10,
            price: 2401.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    let r = b.modify_order(&auth(), &m).await.unwrap();
    assert_eq!(r.order_id, "1300000000000001");
    let body = fake.calls("/modifyorder")[0].json();
    assert_eq!(body["lastmodifiedtime"], "03-Oct-2026 09:20:00");
    assert_eq!(body["qtytradedtoday"], 3);
    assert_eq!(body["newprice"], 2401.0);
    assert_eq!(body["newquantityinlot"], 10);
    assert_eq!(body["newordertype"], "LIMIT");

    // The detail endpoint refuses order 2: the order book is used instead.
    let m2 = ResolvedModify::resolve(
        "1300000000000002",
        &ModifyOrderRequest {
            symbol: "NIFTY28OCT26FUT".into(),
            exchange: "NFO".into(),
            action: "SELL".into(),
            product: "NRML".into(),
            pricetype: "SL".into(),
            quantity: 75,
            price: 101.0,
            trigger_price: 102.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    b.modify_order(&auth(), &m2).await.unwrap();
    let body = fake.calls("/modifyorder")[1].json();
    assert_eq!(body["lastmodifiedtime"], "03-Oct-2026 10:01:00");
    assert_eq!(body["newordertype"], "STOPLOSS");
    assert_eq!(body["newquantityinlot"], 1);
    assert_eq!(fake.calls("/getorderbook").len(), 1);

    let mut missing = m2.clone();
    missing.order_id = "999".into();
    assert!(b.modify_order(&auth(), &missing).await.is_err());
}

#[tokio::test]
async fn cancel_and_cancel_all() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let r = b.cancel_order(&auth(), "1300000000000001").await.unwrap();
    assert_eq!(r.order_id, "1300000000000001");
    assert_eq!(
        fake.calls("/cancelorder")[0].json(),
        json!({"uniqueorderid":"1300000000000001"})
    );
    let e = b
        .cancel_order(&auth(), "1300000000000002")
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), "Motilal Oswal: Order already traded");

    // Confirm and Sent rows are live; Traded, Error and Cancel are not.
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(r.cancelled, ["1300000000000001"]);
    assert_eq!(r.failed, ["1300000000000002"]);
}

#[tokio::test]
async fn close_all_squares_off_each_open_position() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let r = b.close_all_positions(&auth()).await.unwrap();
    assert_eq!(r.placed.len(), 2, "{:?}", r.failed);
    assert!(r.failed.is_empty());
    let orders: Vec<Value> = fake.calls("/placeorder").iter().map(Req::json).collect();
    assert_eq!(orders[0]["buyorsell"], "SELL");
    assert_eq!(orders[0]["producttype"], "VALUEPLUS");
    assert_eq!(orders[0]["quantityinlot"], 10);
    assert_eq!(orders[1]["buyorsell"], "BUY");
    assert_eq!(orders[1]["exchange"], "NSEFO");
    assert_eq!(orders[1]["quantityinlot"], 1);
    // MARKET exits are price protected from the live quote.
    assert_eq!(orders[1]["ordertype"], "LIMIT");
    assert_eq!(orders[1]["price"], 101.0);
}

#[tokio::test]
async fn open_position_matches_token_exchange_and_product() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let q = b
        .get_open_position(&auth(), "RELIANCE", Exchange::Nse, Product::Mis)
        .await
        .unwrap();
    assert_eq!(q, 10);
    let q = b
        .get_open_position(&auth(), "RELIANCE", Exchange::Nse, Product::Cnc)
        .await
        .unwrap();
    assert_eq!(q, 0);
    // F&O: MIS and NRML both trade as NORMAL.
    let q = b
        .get_open_position(&auth(), "NIFTY28OCT26FUT", Exchange::Nfo, Product::Mis)
        .await
        .unwrap();
    assert_eq!(q, -75);
}

#[tokio::test]
async fn books_and_funds() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let o = b.get_order_book(&auth()).await.unwrap();
    assert_eq!(o.len(), 5);
    assert_eq!(
        (o[1].symbol.as_str(), o[1].status.as_str()),
        ("NIFTY28OCT26FUT", "open")
    );
    let t = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(t[0].average_price, 2784.0);
    let p = b.get_positions(&auth()).await.unwrap();
    assert_eq!(p[1].quantity, -75);
    let h = b.get_holdings(&auth()).await.unwrap();
    assert_eq!(h[0].symbol, "RELAXO");
    assert_eq!(fake.calls("/getdpholding")[0].json(), json!({}));
    let f = b.get_funds(&auth()).await.unwrap();
    assert_eq!((f.available_cash, f.collateral), (50000000.0, 474919.06));
    assert!(b.calculate_margin(&auth(), &[]).await.is_err());
}

#[tokio::test]
async fn expired_sessions_are_reported_as_such() {
    let fake = Fake::start(|_req: &Req| {
        ok(json!({"status":"FAILURE","message":"Invalid Token","errorcode":"MO8001","data":null}))
    })
    .await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let e = b.get_funds(&auth()).await.unwrap_err();
    assert!(e.client_message().contains("expired"));
    let e = b.get_order_book(&auth()).await.unwrap_err();
    assert!(e.client_message().contains("expired"));
    assert!(b.get_funds(&AuthToken::new("garbage")).await.is_err());
}

// ---------------------------------------------------------------------------
// Quotes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dealer_logins_retry_with_the_client_code() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let fake = Fake::start(move |req: &Req| {
        if req.json().get("clientcode").is_none() {
            c.fetch_add(1, Ordering::SeqCst);
            return ok(json!({"status":"FAILURE","message":"Please Provide Client Code In Input Parameter","errorcode":"MO1062"}));
        }
        ok(crate::fixture!("motilal", "ltp.json"))
    })
    .await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let q = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    assert_eq!((q.ltp, q.bid, q.close), (3224.0, 3230.0, 3210.0));
    // Dealer mode is remembered: the next call sends clientcode at once.
    b.get_quote(&auth(), &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let all = fake.calls("/getltpdata");
    assert_eq!(all.len(), 3);
    assert_eq!(all[2].json()["clientcode"], "<USER_ID>");
}

#[tokio::test]
async fn index_quotes_learn_the_exchange_field() {
    let fake = Fake::start(|req: &Req| {
        let b = req.json();
        if b.get("exchangename").is_some() {
            return ok(json!({"status":"FAILURE","message":"Invalid Exchange","errorcode":"MO1051"}));
        }
        ok(json!({"status":"SUCCESS","data":[{"scripcode":"26000","open":25000.5,"high":25100,"low":24900,"close":24950,"ltp":25050}]}))
    })
    .await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let k = QuoteKey::new("NSE_INDEX", "NIFTY");
    let q = b.get_quote(&auth(), &k).await.unwrap();
    assert_eq!((q.ltp, q.open, q.close), (25050.0, 25000.5, 24950.0));
    b.get_quote(&auth(), &k).await.unwrap();
    let calls = fake.calls("/getindexltpdata");
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[1].json(),
        json!({"exchange":"NSE","scripcode":"26000"})
    );
    assert!(calls[2].json().get("exchangename").is_none());
    // MCX indices are not offered by the index API.
    let e = b
        .get_quote(&auth(), &QuoteKey::new("MCX_INDEX", "GOLDIDX"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("NSE and BSE indices only"));
}

// ---------------------------------------------------------------------------
// Broadcast socket (multiquotes, depth)
// ---------------------------------------------------------------------------

fn pkt(ex: u8, scrip: i32, kind: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![ex];
    p.extend_from_slice(&scrip.to_le_bytes());
    p.extend_from_slice(&0i32.to_le_bytes());
    p.push(kind);
    let mut b = body.to_vec();
    b.resize(20, 0);
    p.extend(b);
    p
}

fn snapshot(ex: u8, scrip: i32, price: f32) -> Vec<u8> {
    let mut f = Vec::new();
    let mut a = Vec::new();
    a.extend(price.to_le_bytes());
    a.extend(5i32.to_le_bytes());
    a.extend(1000i32.to_le_bytes());
    a.extend(price.to_le_bytes());
    a.extend(0i32.to_le_bytes());
    f.extend(pkt(ex, scrip, b'A', &a));
    let mut g = Vec::new();
    for v in [price - 10.0, price + 10.0, price - 20.0, price - 5.0] {
        g.extend(v.to_le_bytes());
    }
    f.extend(pkt(ex, scrip, b'G', &g));
    for (i, kind) in [b'B', b'C', b'D', b'E', b'F'].into_iter().enumerate() {
        let step = (i + 1) as f32 * 0.05;
        let mut d = Vec::new();
        d.extend((price - step).to_le_bytes());
        d.extend((100 * (i as i32 + 1)).to_le_bytes());
        d.extend(2i16.to_le_bytes());
        d.extend((price + step).to_le_bytes());
        d.extend((50 * (i as i32 + 1)).to_le_bytes());
        d.extend(1i16.to_le_bytes());
        f.extend(pkt(ex, scrip, kind, &d));
    }
    f.extend(pkt(
        ex,
        scrip,
        b'm',
        &[4500i32.to_le_bytes(), 0i32.to_le_bytes()].concat(),
    ));
    f
}

/// Wait (bounded) for the fake server to record what the client sent.
async fn settle(done: impl Fn() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[derive(Default)]
struct Seen {
    logins: AtomicUsize,
    registers: Mutex<Vec<Vec<u8>>>,
    texts: Mutex<Vec<String>>,
}

/// A broadcast server speaking the binary protocol: answers the login with
/// a heartbeat packet, a scrip registration with a full snapshot, and an
/// index registration with an index packet for NIFTY.
async fn feed_server(seen: Arc<Seen>) -> FakeWs {
    FakeWs::start(move |mut ws: ServerWs| {
        let seen = seen.clone();
        async move {
            while let Some(Ok(m)) = ws.next().await {
                match m {
                    Message::Binary(b) if b.len() == 114 => {
                        seen.logins.fetch_add(1, Ordering::SeqCst);
                        let _ = ws.send(Message::Binary(pkt(b'N', 0, b'1', &[]))).await;
                    }
                    Message::Binary(b) if b.len() == 10 => {
                        seen.registers.lock().push(b.to_vec());
                        if b[9] == 1 {
                            let scrip = i32::from_le_bytes([b[5], b[6], b[7], b[8]]);
                            let price = if scrip == 35001 { 101.5 } else { 2410.5 };
                            let _ = ws.send(Message::Binary(snapshot(b[3], scrip, price))).await;
                        }
                    }
                    Message::Text(t) => {
                        if t.contains("IndexRegister") {
                            let _ = ws
                                .send(Message::Binary(pkt(
                                    b'N',
                                    26000,
                                    b'H',
                                    &25012.75f32.to_le_bytes(),
                                )))
                                .await;
                        }
                        seen.texts.lock().push(t.to_string());
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    })
    .await
}

#[tokio::test]
async fn multiquotes_over_the_broadcast_socket() {
    let seen = Arc::new(Seen::default());
    let ws = feed_server(seen.clone()).await;
    let b = broker("http://127.0.0.1:9", &ws.url);
    let keys = vec![
        QuoteKey::new("NSE", "RELIANCE"),
        QuoteKey::new("NFO", "NIFTY28OCT26FUT"),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("MCX_INDEX", "GOLDIDX"),
    ];
    let r = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(r.len(), 4);
    let q = r[0].data.as_ref().unwrap();
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.volume),
        (2410.5, 2410.45, 2410.55, 1000)
    );
    assert_eq!((q.open, q.close), (2400.5, 2405.5));
    let f = r[1].data.as_ref().unwrap();
    assert_eq!((f.ltp, f.oi), (101.5, 4500));
    assert_eq!(r[2].error.as_deref(), Some("Could not resolve token"));
    assert!(r[3].error.as_deref().unwrap().contains("NSE and BSE"));
    // One socket: login, two registers, two unregisters (the server may
    // still be reading the last frames when the call returns).
    settle(|| seen.registers.lock().len() >= 4).await;
    assert_eq!(seen.logins.load(Ordering::SeqCst), 1);
    let regs = seen.registers.lock().clone();
    assert_eq!(regs.len(), 4);
    assert_eq!((regs[1][3], regs[1][4]), (b'N', b'D'));
    assert_eq!(regs.iter().filter(|p| p[9] == 0).count(), 2);
    assert_eq!(ws.handshakes.lock().len(), 1);
}

#[tokio::test]
async fn index_multiquote_uses_index_register() {
    let seen = Arc::new(Seen::default());
    let ws = feed_server(seen.clone()).await;
    let b = broker("http://127.0.0.1:9", &ws.url);
    let r = b
        .get_multiquotes(&auth(), &[QuoteKey::new("NSE_INDEX", "NIFTY")])
        .await
        .unwrap();
    assert_eq!(r[0].data.as_ref().unwrap().ltp, 25012.75);
    settle(|| seen.texts.lock().len() >= 2).await;
    let texts = seen.texts.lock().clone();
    assert!(texts[0].contains("IndexRegister") && texts[0].contains("\"NSE\""));
    assert!(texts[1].contains("IndexUnregister"));
}

#[tokio::test]
async fn depth_over_the_broadcast_socket() {
    let seen = Arc::new(Seen::default());
    let ws = feed_server(seen.clone()).await;
    let b = broker("http://127.0.0.1:9", &ws.url);
    let d = b
        .get_market_depth(&auth(), &QuoteKey::new("NFO", "NIFTY28OCT26FUT"))
        .await
        .unwrap();
    assert_eq!((d.bids.len(), d.asks.len()), (5, 5));
    assert_eq!((d.bids[0].price, d.bids[0].quantity), (101.45, 100));
    assert_eq!((d.asks[4].price, d.asks[4].quantity), (101.75, 250));
    assert_eq!((d.ltp, d.ltq, d.oi, d.volume), (101.5, 5, 4500, 1000));
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (0, 0));

    let i = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(i.ltp, 25012.75);
    assert!(i.bids.iter().all(|l| l.price == 0.0));
}

#[tokio::test]
async fn unreachable_feed_is_a_trader_message() {
    let b = broker("http://127.0.0.1:9", "ws://127.0.0.1:9");
    let e = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("not reachable"));
}

// ---------------------------------------------------------------------------
// History and master contract
// ---------------------------------------------------------------------------

#[tokio::test]
async fn history_is_todays_daily_bar_only() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let today = (chrono::Utc::now() + chrono::Duration::minutes(330)).date_naive();
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        interval: "D".into(),
        start: today - chrono::Duration::days(5),
        end: today,
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].close, 2400.0);
    assert_eq!(c[0].timestamp % 86400, 0);

    let past = HistoryRequest {
        end: today - chrono::Duration::days(1),
        ..req.clone()
    };
    assert!(b.get_history(&auth(), &past).await.unwrap().is_empty());
    let intraday = HistoryRequest {
        interval: "5m".into(),
        ..req
    };
    let e = b.get_history(&auth(), &intraday).await.unwrap_err();
    assert!(e.client_message().contains("Only the daily interval"));
}

#[tokio::test]
async fn master_contract_download() {
    let fake = Fake::start(|req: &Req| {
        let name = req.param("name").unwrap_or_default();
        if req.path.ends_with("/getscripmastercsv") {
            return match name.as_str() {
                "NSE" => text(crate::fixture!("motilal", "scrip_nse.csv")),
                "BSE" => text(crate::fixture!("motilal", "scrip_bse.csv")),
                "NSEFO" => text(crate::fixture!("motilal", "scrip_nsefo.csv")),
                "NSECD" => text(crate::fixture!("motilal", "scrip_nsecd.csv")),
                "MCX" => text(crate::fixture!("motilal", "scrip_mcx.csv")),
                // One exchange failing does not fail the download.
                _ => with_status(StatusCode::BAD_GATEWAY, "{}"),
            };
        }
        match name.as_str() {
            "NSE" => text(crate::fixture!("motilal", "index_nse.csv")),
            _ => text(crate::fixture!("motilal", "index_bse.csv")),
        }
    })
    .await;
    let b = broker(&fake.base, "ws://127.0.0.1:9");
    let rows = b.download_master_contract(&auth()).await.unwrap();
    let has = |s: &str, e: &str| rows.iter().any(|r| r.symbol == s && r.exchange == e);
    assert!(has("INFY", "NSE"));
    assert!(has("NIFTY28OCT2625000CE", "NFO"));
    assert!(has("USDINR23OCT26FUT", "CDS"));
    assert!(has("CRUDEOIL19OCT26FUT", "MCX"));
    assert!(has("BANKNIFTY", "NSE_INDEX"));
    assert!(has("SENSEX50", "BSE_INDEX"));
    assert!(!rows.iter().any(|r| r.exchange == "BFO"));
    assert_eq!(
        rows.iter()
            .filter(|r| r.symbol == "NIFTY" && r.exchange == "NSE_INDEX")
            .count(),
        1
    );
    assert_eq!(fake.calls("/getscripmastercsv").len(), 6);
    assert_eq!(fake.calls("/getindexdatacsv").len(), 2);

    let dead = Fake::start(|_req: &Req| with_status(StatusCode::BAD_GATEWAY, "{}")).await;
    let b = broker(&dead.base, "ws://127.0.0.1:9");
    assert!(b.download_master_contract(&auth()).await.is_err());
}

/// Sentinel credentials and session through every sign-in and request
/// error path (refusing broker, unreachable broker): the sentinel never
/// reaches a log line, an error's Display/Debug or a trader message.
#[tokio::test]
async fn secrets_stay_out_of_errors_and_logs() {
    let logs = capture_logs();
    let fake = refusing_fake().await;
    let key = QuoteKey::new("NSE", "SBIN");
    for base in [fake.base.clone(), closed_base()] {
        let b = MotilalBroker::with_urls(master(), &base, closed_ws(), closed_ws())
            .with_timings(fast());
        clean_err(b.authenticate(sentinel_creds()).await);
        let auth = AuthToken::new(format!("{s}:::{s}:::{s}:::{s}:::{s}", s = SENTINEL))
            .with_feed(Some(SENTINEL))
            .with_user_id(SENTINEL);
        clean_err(b.get_order_book(&auth).await);
        clean_err(b.get_positions(&auth).await);
        clean_err(b.get_funds(&auth).await);
        clean_err(b.cancel_order(&auth, "1").await);
        clean_err(b.get_quote(&auth, &key).await);
        clean_err(b.get_market_depth(&auth, &key).await);
        clean_err(b.download_master_contract(&auth).await);
    }
    assert!(!logs.text().is_empty(), "the log capture saw nothing");
    logs.assert_clean();
}
