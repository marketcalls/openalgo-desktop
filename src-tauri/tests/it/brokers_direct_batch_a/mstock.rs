//! mstock against a local fake broker: both sign-in steps and their
//! failures, order bodies and HTTP-200 refusals, cancel-all, close-all,
//! books, funds, margin, quotes, depth over a fake market-data socket,
//! history chunking and the today split, and the master download.

#![allow(unused_imports)]

use super::support::*;
use crate::fixture;
use openalgo_desktop_lib::brokers::mstock::master_contract::build;
use openalgo_desktop_lib::brokers::mstock::MstockBroker;
use std::time::Duration;

const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.e30.sig";
const PKEY: &str = "PRIVATEKEY123";

fn symbols() -> SymbolResolver {
    let rows: Vec<Value> = serde_json::from_str(fixture!("mstock", "scrip_master.json")).unwrap();
    let r = SymbolResolver::new();
    r.load(build(
        &rows,
        Some(fixture!("mstock", "annexure.html")),
        2026,
    ));
    r
}

fn auth() -> AuthToken {
    AuthToken::new(format!("{}:::{}", JWT, PKEY))
}

fn broker(fake: &Fake, ws: &str) -> MstockBroker {
    MstockBroker::with_urls(
        symbols(),
        format!("{}/openapi/typeb", fake.base),
        format!("{}/docs/v1/Annexure/", fake.base),
        ws,
    )
    .with_data_pacing(Duration::from_millis(1))
    .with_depth_timeout(Duration::from_millis(1500))
    .with_today(d(2026, 10, 3))
}

fn route(r: &Req) -> Response {
    let p = r.path.trim_start_matches("/openapi/typeb");
    match (r.method.as_str(), p) {
        ("POST", "/connect/login") => ok(fixture!("mstock", "login.json")),
        ("POST", "/session/verifytotp") => ok(fixture!("mstock", "verifytotp.json")),
        ("POST", "/orders/regular") => {
            let b = r.json();
            if b["tradingsymbol"] == "INFY-EQ" {
                ok(
                    json!({"status": "false", "message": "RMS:Margin Exceeds", "errorcode": "RMS01", "data": null}),
                )
            } else if b["exchange"] == "NFO" {
                ok(json!([{"status": true, "message": "SUCCESS", "data": {"orderid": "2002"}}]))
            } else {
                ok(json!({"status": "true", "message": "SUCCESS", "data": {"orderid": "2001"}}))
            }
        }
        ("PUT", p) if p.starts_with("/orders/regular/") => {
            ok(json!({"status": true, "message": "SUCCESS", "data": {"orderid": "1181251003101 "}}))
        }
        ("DELETE", "/orders/regular/bad") => {
            ok(json!({"status": false, "message": "Order already cancelled"}))
        }
        ("DELETE", p) if p.starts_with("/orders/regular/") => {
            ok(json!({"status": false, "message": "SUCCESS", "data": null}))
        }
        ("POST", "/orders/cancelall") => ok(json!({"status": "true", "message": "SUCCESS"})),
        ("GET", "/orders") => ok(fixture!("mstock", "order_book.json")),
        ("GET", "/tradebook") => ok(fixture!("mstock", "trade_book.json")),
        ("GET", "/portfolio/positions") => ok(fixture!("mstock", "positions.json")),
        ("GET", "/portfolio/holdings") => ok(fixture!("mstock", "holdings.json")),
        ("GET", "/user/fundsummary") => ok(fixture!("mstock", "fund_summary.json")),
        ("POST", "/margins/orders") => ok(fixture!("mstock", "margin.json")),
        ("GET", "/instruments/quote") => ok(fixture!("mstock", "quote.json")),
        ("GET", "/instruments/historical") => ok(fixture!("mstock", "historical.json")),
        ("POST", "/instruments/intraday") => ok(fixture!("mstock", "intraday.json")),
        ("GET", "/instruments/OpenAPIScripMaster") => ok(fixture!("mstock", "scrip_master.json")),
        ("GET", "/docs/v1/Annexure/") => text(fixture!("mstock", "annexure.html")),
        _ => with_status(
            StatusCode::NOT_FOUND,
            json!({"status": false, "message": "no route"}),
        ),
    }
}

fn creds(password: &str, totp: &str) -> BrokerCredentials {
    BrokerCredentials {
        api_key: "MA12345".into(),
        api_secret: Some(PKEY.into()),
        password: Some(password.into()),
        totp: Some(totp.into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn login_runs_both_steps() {
    let fake = Fake::start(route).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let r = b.authenticate(creds("pw", "123456")).await.unwrap();
    assert_eq!(r.auth_token, format!("<JWT>:::{}", PKEY));
    assert_eq!(r.feed_token.as_deref(), Some("<FEED_TOKEN>"));
    assert_eq!(r.user_id, "MA12345");
    let login = &fake.calls("/connect/login")[0];
    assert_eq!(login.header("x-mirae-version"), "1");
    assert_eq!(login.header("x-privatekey"), "");
    assert_eq!(
        login.json(),
        json!({"clientcode": "MA12345", "password": "pw", "totp": "123456", "state": ""})
    );
    let verify = &fake.calls("/session/verifytotp")[0];
    assert_eq!(verify.header("x-privatekey"), PKEY);
    assert_eq!(
        verify.json(),
        json!({"refreshToken": "<REFRESH_TOKEN>", "totp": "123456"})
    );
    // Missing inputs never reach the broker.
    let e = b.authenticate(creds("", "1")).await.unwrap_err();
    assert_eq!(e.client_message(), "Password is required.");
    let mut c = creds("pw", "1");
    c.api_secret = None;
    assert!(b.authenticate(c).await.is_err());
    assert_eq!(fake.calls("/connect/login").len(), 1);
}

#[tokio::test]
async fn login_step_one_refused() {
    let fake = Fake::start(|r: &Req| {
        if r.path.ends_with("/connect/login") {
            ok(json!({"status": false, "message": "Invalid Password", "data": null}))
        } else {
            route(r)
        }
    })
    .await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let e = b.authenticate(creds("bad", "123456")).await.unwrap_err();
    assert!(
        e.client_message().contains("Invalid Password"),
        "{}",
        e.client_message()
    );
    assert!(fake.calls("/session/verifytotp").is_empty());
}

#[tokio::test]
async fn login_step_two_refused() {
    let fake = Fake::start(|r: &Req| {
        if r.path.ends_with("/session/verifytotp") {
            with_status(
                StatusCode::BAD_REQUEST,
                json!({"status": "false", "message": "Invalid TOTP"}),
            )
        } else {
            route(r)
        }
    })
    .await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let e = b.authenticate(creds("pw", "000000")).await.unwrap_err();
    assert!(
        e.client_message().contains("Invalid TOTP"),
        "{}",
        e.client_message()
    );
}

fn order(symbol: &str, exchange: &str, side: &str, pricetype: &str, product: &str) -> OrderRequest {
    OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: side.into(),
        quantity: 10,
        price: if pricetype == "LIMIT" { 750.5 } else { 0.0 },
        order_type: pricetype.into(),
        product: product.into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    }
}

#[tokio::test]
async fn orders_and_refusals() {
    let fake = Fake::start(route).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let s = symbols();
    let o = ResolvedOrder::resolve(&order("SBIN", "NSE", "BUY", "LIMIT", "CNC"), &s).unwrap();
    let r = b.place_order(&auth(), &o).await.unwrap();
    assert_eq!(r.order_id, "2001");
    let call = &fake.calls("/orders/regular")[0];
    assert_eq!(call.header("authorization"), format!("Bearer {}", JWT));
    assert_eq!(call.header("x-privatekey"), PKEY);
    assert_eq!(call.header("x-mirae-version"), "1");
    let body = call.json();
    assert_eq!(body["tradingsymbol"], "SBIN-EQ");
    assert_eq!(body["symboltoken"], "3045");
    assert_eq!(body["producttype"], "DELIVERY");
    assert_eq!(body["price"], "750.5");
    // The one-element list answer is unwrapped.
    let o = ResolvedOrder::resolve(
        &order("NIFTY27OCT2624500CE", "NFO", "SELL", "MARKET", "NRML"),
        &s,
    )
    .unwrap();
    assert_eq!(b.place_order(&auth(), &o).await.unwrap().order_id, "2002");
    // HTTP 200 with status false is a refusal carrying mStock's reason.
    let o = ResolvedOrder::resolve(&order("INFY", "NSE", "BUY", "MARKET", "MIS"), &s).unwrap();
    let e = b.place_order(&auth(), &o).await.unwrap_err();
    assert_eq!(e.client_message(), "mStock: RMS:Margin Exceeds");

    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "CNC".into(),
        pricetype: "LIMIT".into(),
        quantity: 10,
        price: 751.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("1181251003101", &m, &s).unwrap();
    let r = b.modify_order(&auth(), &rm).await.unwrap();
    assert_eq!(r.order_id, "1181251003101");
    let put = fake
        .all()
        .into_iter()
        .find(|r| r.method == Method::PUT)
        .unwrap();
    assert!(put.path.ends_with("/orders/regular/1181251003101"));
    assert_eq!(put.json()["modqty_remng"], "0");
    assert_eq!(put.json()["price"], "751");

    // Cancel: message SUCCESS counts even with status false (web rule).
    let r = b.cancel_order(&auth(), "1181251003101").await.unwrap();
    assert_eq!(r.order_id, "1181251003101");
    let del = fake
        .all()
        .into_iter()
        .find(|r| r.method == Method::DELETE)
        .unwrap();
    assert_eq!(
        del.json(),
        json!({"variety": "NORMAL", "orderid": "1181251003101"})
    );
    let e = b.cancel_order(&auth(), "bad").await.unwrap_err();
    assert_eq!(e.client_message(), "mStock: Order already cancelled");
}

#[tokio::test]
async fn expired_session_is_an_auth_error() {
    let fake = Fake::start(|_: &Req| {
        with_status(StatusCode::UNAUTHORIZED, json!({"message": "Unauthorized"}))
    })
    .await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let e = b.get_order_book(&auth()).await.unwrap_err();
    assert!(e.client_message().contains("session has expired"));
    let e = b
        .get_funds(&AuthToken::new("no-separator"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("session has expired"));
}

#[tokio::test]
async fn cancel_all_close_all_and_open_position() {
    let fake = Fake::start(route).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(r.cancelled, ["1181251003101", "1181251003103"]);
    assert!(r.failed.is_empty());
    assert_eq!(fake.calls("/orders/cancelall").len(), 1);

    let r = b.close_all_positions(&auth()).await.unwrap();
    assert_eq!(r.placed.len(), 2, "{:?}", r);
    assert!(r.failed.is_empty());
    let places = fake.calls("/orders/regular");
    let sbin = places
        .iter()
        .find(|c| c.json()["symboltoken"] == "3045")
        .unwrap()
        .json();
    assert_eq!(
        (
            sbin["transactiontype"].as_str(),
            sbin["quantity"].as_str(),
            sbin["producttype"].as_str(),
            sbin["ordertype"].as_str()
        ),
        (Some("SELL"), Some("10"), Some("DELIVERY"), Some("MARKET"))
    );
    let opt = places
        .iter()
        .find(|c| c.json()["symboltoken"] == "45001")
        .unwrap()
        .json();
    assert_eq!(opt["exchange"], "NFO");
    assert_eq!(opt["transactiontype"], "BUY");
    assert_eq!(opt["quantity"], "75");
    assert_eq!(opt["producttype"], "CARRYFORWARD");

    let q = b
        .get_open_position(&auth(), "NIFTY27OCT2624500CE", Exchange::Nfo, Product::Nrml)
        .await
        .unwrap();
    assert_eq!(q, -75);
    let q = b
        .get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Cnc)
        .await
        .unwrap();
    assert_eq!(q, 10);
    let q = b
        .get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Mis)
        .await
        .unwrap();
    assert_eq!(q, 0);
}

#[tokio::test]
async fn cancel_all_with_nothing_pending_sends_nothing() {
    let fake = Fake::start(|r: &Req| {
        if r.path.ends_with("/orders") {
            ok(json!({"status": "true", "data": [{"orderid": "1", "status": "Traded"}]}))
        } else {
            route(r)
        }
    })
    .await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    assert!(r.cancelled.is_empty() && r.failed.is_empty());
    assert!(fake.calls("/orders/cancelall").is_empty());
}

#[tokio::test]
async fn books_funds_and_margin() {
    let fake = Fake::start(route).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let o = b.get_order_book(&auth()).await.unwrap();
    assert_eq!(o.len(), 5);
    assert_eq!(o[1].symbol, "NIFTY27OCT2624500CE");
    let t = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(t[1].symbol, "INFY");
    let p = b.get_positions(&auth()).await.unwrap();
    assert_eq!(p[1].exchange, "NFO");
    let h = b.get_holdings(&auth()).await.unwrap();
    assert_eq!(h[0].symbol, "SBIN");
    let f = b.get_funds(&auth()).await.unwrap();
    assert_eq!((f.available_cash, f.utilised_debits), (100000.46, 2500.5));

    let legs = vec![
        MarginLeg {
            key: QuoteKey::new("NFO", "NIFTY27OCT2624500CE"),
            action: Action::Sell,
            quantity: 75,
            product: Product::Nrml,
            pricetype: PriceType::Market,
            price: 0.0,
            trigger_price: 0.0,
        },
        MarginLeg {
            key: QuoteKey::new("NFO", "NOPE"),
            action: Action::Buy,
            quantity: 1,
            product: Product::Nrml,
            pricetype: PriceType::Market,
            price: 0.0,
            trigger_price: 0.0,
        },
    ];
    let m = b.calculate_margin(&auth(), &legs).await.unwrap();
    assert_eq!(m.total_margin_required, 152340.75);
    assert_eq!(m.span_margin, 120000.5);
    let body = fake.calls("/margins/orders")[0].json();
    assert_eq!(body["orders"].as_array().unwrap().len(), 1);
    assert_eq!(body["orders"][0]["symbol_name"], "NIFTY26OCT24500CE");
    assert_eq!(body["orders"][0]["product_type"], "CARRYFORWARD");
    let e = b.calculate_margin(&auth(), &legs[1..]).await.unwrap_err();
    assert!(e.client_message().contains("No valid positions"));
}

#[tokio::test]
async fn quotes_and_multiquotes() {
    let fake = Fake::start(route).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let q = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((q.ltp, q.close), (752.4, 748.0));
    let call = &fake.calls("/instruments/quote")[0];
    assert_eq!(call.method, Method::GET);
    assert_eq!(
        call.json(),
        json!({"mode": "OHLC", "exchangeTokens": {"NSE": ["3045"]}})
    );
    let keys = vec![
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NSE", "INFY"),
        QuoteKey::new("NFO", "NIFTY27OCT2624500CE"),
    ];
    let r = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(r.len(), 4);
    assert_eq!(r[0].data.as_ref().unwrap().ltp, 752.4);
    assert_eq!(r[1].error.as_deref(), Some("Could not resolve token"));
    assert_eq!(r[2].data.as_ref().unwrap().volume, 120000);
    assert_eq!(r[3].error.as_deref(), Some("No data received"));
    let body = fake.calls("/instruments/quote")[1].json();
    assert_eq!(
        body["exchangeTokens"],
        json!({"NSE": ["3045", "1594"], "NFO": ["45001"]})
    );
}

fn snap(token: &str) -> Vec<u8> {
    let mut p = vec![0u8; 379];
    p[0] = 3;
    p[1] = 1;
    p[2..2 + token.len()].copy_from_slice(token.as_bytes());
    p[43..51].copy_from_slice(&75_240u64.to_le_bytes());
    p[51..59].copy_from_slice(&25u64.to_le_bytes());
    p[115..123].copy_from_slice(&74_800u64.to_le_bytes());
    for i in 0..10usize {
        let o = 147 + i * 20;
        p[o + 2..o + 10].copy_from_slice(&(100 + i as u64).to_le_bytes());
        p[o + 10..o + 18].copy_from_slice(&(75_000 + 5 * i as u64).to_le_bytes());
        p[o + 18..o + 20].copy_from_slice(&(1 + i as u16).to_le_bytes());
    }
    p
}

#[tokio::test]
async fn depth_over_a_one_shot_socket() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = seen.clone();
    let ws = FakeWs::start(move |mut ws| {
        let rec = rec.clone();
        async move {
            while let Some(t) = next_text(&mut ws).await {
                rec.lock().push(t.clone());
                if t.contains("\"action\":1") {
                    // Something else first, then the snap packet behind a header.
                    send_text(&mut ws, "ok").await;
                    let mut frame = Vec::new();
                    frame.extend(1u16.to_le_bytes());
                    frame.extend(379u16.to_le_bytes());
                    frame.extend(snap("3045"));
                    send_binary(&mut ws, frame).await;
                }
            }
        }
    })
    .await;
    let fake = Fake::start(route).await;
    let b = broker(&fake, &ws.url);
    let d = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((d.ltp, d.ltq, d.prev_close), (752.4, 25, 748.0));
    assert_eq!(d.bids.len(), 5);
    assert_eq!((d.bids[0].price, d.bids[0].quantity), (750.0, 100));
    assert_eq!(d.asks[0].price, 750.25);
    let hs = ws.handshakes.lock().clone();
    assert_eq!(hs[0].0, format!("/?API_KEY={}&ACCESS_TOKEN={}", PKEY, JWT));
    let sent = seen.lock().clone();
    assert_eq!(sent[0], format!("LOGIN:{}", JWT));
    assert_eq!(
        serde_json::from_str::<Value>(&sent[1]).unwrap(),
        json!({"action": 1, "params": {"mode": 3, "tokenList": [{"exchangeType": 1, "tokens": ["3045"]}]}})
    );
}

#[tokio::test]
async fn depth_without_an_answer_times_out() {
    let ws =
        FakeWs::start(|mut ws| async move { while next_text(&mut ws).await.is_some() {} }).await;
    let fake = Fake::start(route).await;
    let b = broker(&fake, &ws.url).with_depth_timeout(Duration::from_millis(300));
    let started = std::time::Instant::now();
    let e = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("did not send market depth"));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn history_chunks_and_today_split() {
    let fake = Fake::start(route).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    // Past range, 1m: two-day chunks.
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "1m".into(),
        start: d(2026, 9, 21),
        end: d(2026, 9, 24),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].timestamp, 1_790_653_500);
    let calls = fake.calls("/instruments/historical");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].method, Method::GET);
    assert_eq!(
        calls[0].json(),
        json!({"exchange": "NSE", "symboltoken": "3045", "interval": "ONE_MINUTE",
               "fromdate": "2026-09-21 00:00", "todate": "2026-09-22 23:59"})
    );
    assert_eq!(calls[1].json()["fromdate"], "2026-09-23 00:00");
    assert!(fake.calls("/instruments/intraday").is_empty());

    // Range ending today: history to yesterday plus today's intraday.
    let req = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "5m".into(),
        start: d(2026, 10, 1),
        end: d(2026, 10, 3),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 4);
    assert!(c.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
    let h = fake.calls("/instruments/historical");
    assert_eq!(h[2].json()["todate"], "2026-10-02 23:59");
    assert_eq!(h[2].json()["exchange"], "NSE");
    let i = fake.calls("/instruments/intraday");
    assert_eq!(
        i[0].json(),
        json!({"exchange": "1", "symboltoken": "26000", "interval": "FIVE_MINUTE"})
    );

    // Today only: intraday alone.
    let before = fake.calls("/instruments/historical").len();
    let req = HistoryRequest {
        key: QuoteKey::new("NFO", "NIFTY27OCT2624500CE"),
        interval: "1h".into(),
        start: d(2026, 10, 3),
        end: d(2026, 10, 3),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(fake.calls("/instruments/historical").len(), before);
    assert_eq!(
        fake.calls("/instruments/intraday")[1].json()["exchange"],
        "2"
    );

    let bad = HistoryRequest {
        interval: "W".into(),
        ..req
    };
    let e = b.get_history(&auth(), &bad).await.unwrap_err();
    assert!(e.client_message().contains("not supported by mStock"));
}

#[tokio::test]
async fn master_download_with_and_without_the_index_page() {
    let fake = Fake::start(route).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let rows = b.download_master_contract(&auth()).await.unwrap();
    assert!(rows
        .iter()
        .any(|r| r.exchange == "NSE_INDEX" && r.symbol == "NIFTY"));
    assert!(rows
        .iter()
        .any(|r| r.exchange == "BSE_INDEX" && r.symbol == "SENSEX"));
    let call = &fake.calls("/instruments/OpenAPIScripMaster")[0];
    assert_eq!(call.header("x-privatekey"), PKEY);

    let fake2 = Fake::start(|r: &Req| {
        if r.path.contains("Annexure") {
            with_status(StatusCode::NOT_FOUND, "gone")
        } else {
            route(r)
        }
    })
    .await;
    let b2 = broker(&fake2, "ws://127.0.0.1:9");
    let rows = b2.download_master_contract(&auth()).await.unwrap();
    assert!(!rows.iter().any(|r| r.exchange.ends_with("_INDEX")));
    assert!(rows.iter().any(|r| r.symbol == "NIFTY27OCT2624500CE"));
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
        let b = MstockBroker::with_urls(
            symbols(),
            format!("{}/openapi/typeb", base),
            format!("{}/docs/v1/Annexure/", base),
            closed_ws(),
        )
        .with_data_pacing(Duration::from_millis(1))
        .with_depth_timeout(Duration::from_millis(500));
        clean_err(b.authenticate(sentinel_creds()).await);
        let auth = AuthToken::new(format!("{s}:::{s}", s = SENTINEL))
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
