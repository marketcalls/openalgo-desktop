//! Samco against a local fake broker: v3.2 key/secret sign-in and its
//! error codes, the static IP diagnostic, orders with market price
//! protection, cancel-all, close-all over the DAY and NET books, books,
//! funds, span margin, quotes, index quotes, multiquote batching, depth,
//! daily and intraday history, and the scrip master download.

#![allow(unused_imports)]

use super::support::*;
use openalgo_desktop_lib::brokers::samco::{self, SamcoBroker};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    let mut opt = row(
        "NIFTY28OCT2624600CE",
        "NIFTY26OCT24600CE",
        "NFO",
        "NFO",
        "41015_NFO",
        75,
        0.05,
    );
    opt.instrument_type = "CE".into();
    let mut nifty = row("NIFTY", "NIFTY 50", "NSE_INDEX", "NSE", "NIFTY_50", 1, 0.05);
    nifty.instrument_type = "INDEX".into();
    r.load(vec![
        row("SBIN", "SBIN-EQ", "NSE", "NSE", "3045_NSE", 1, 0.05),
        opt,
        row(
            "CRUDEOIL20OCT26FUT",
            "CRUDEOIL26OCTFUT",
            "MCX",
            "MFO",
            "464925_MFO",
            100,
            1.0,
        ),
        nifty,
    ]);
    r
}

fn broker(fake: &Fake) -> SamcoBroker {
    SamcoBroker::with_urls(
        master(),
        &fake.base,
        format!("{}/doc/ScripMaster.csv", fake.base),
        "ws://127.0.0.1:9",
    )
    .with_timing(Duration::from_millis(5), Duration::from_millis(1))
    .with_today(d(2026, 10, 3))
}

fn auth() -> AuthToken {
    AuthToken::new("<SESSION_TOKEN>")
}

fn creds(key: &str, secret: Option<&str>) -> BrokerCredentials {
    BrokerCredentials {
        api_key: key.into(),
        api_secret: secret.map(Into::into),
        ..Default::default()
    }
}

/// The default fake Samco.
fn samco_routes(r: &Req) -> Response {
    let ok_status = || ok(json!({"status":"Success","statusMessage":"done"}));
    match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/session/token") => {
            let b = r.json();
            match (b["apiKey"].as_str(), b["apiSecret"].as_str()) {
                (Some("KEY"), Some("SECRET")) => ok(crate::fixture!("samco", "session_token.json")),
                (Some("KEY"), _) => ok(
                    json!({"status":"Failure","statusMessage":"Invalid API secret","errorCode":"EOAUTH008"}),
                ),
                (Some("BLOCKED"), _) => ok(
                    json!({"status":"Failure","statusMessage":"IP not registered","errorCode":"EOAUTH009"}),
                ),
                _ => ok(
                    json!({"status":"Failure","statusMessage":"Invalid API key","errorCode":"EOAUTH001"}),
                ),
            }
        }
        ("GET", "/ip/whoami") => ok(crate::fixture!("samco", "whoami.json")),
        ("POST", "/order/placeOrder") => ok(
            json!({"status":"Success","statusMessage":"Order placed","orderNumber":"261003000000201"}),
        ),
        ("PUT", p) if p.starts_with("/order/modifyOrder/") => ok(
            json!({"status":"Success","statusMessage":"Order modified","ordernumber":"261003000000101"}),
        ),
        ("DELETE", "/order/cancelOrder") => {
            if r.param("orderNumber").as_deref() == Some("261003000000103") {
                ok(json!({"status":"Failure","statusMessage":"Order already cancelled"}))
            } else {
                ok_status()
            }
        }
        ("GET", "/order/orderBook") => ok(crate::fixture!("samco", "order_book.json")),
        ("GET", "/trade/tradeBook") => ok(crate::fixture!("samco", "trade_book.json")),
        ("GET", "/position/getPositions") => match r.param("positionType").as_deref() {
            Some("NET") => ok(crate::fixture!("samco", "positions_net.json")),
            _ => ok(crate::fixture!("samco", "positions_day.json")),
        },
        ("GET", "/holding/getHoldings") => ok(crate::fixture!("samco", "holdings.json")),
        ("GET", "/limit/getLimits") => ok(crate::fixture!("samco", "limits.json")),
        ("POST", "/spanMargin") => ok(crate::fixture!("samco", "span_margin.json")),
        ("GET", "/quote/getQuote") => ok(crate::fixture!("samco", "quote.json")),
        ("GET", "/quote/indexQuote") => ok(crate::fixture!("samco", "index_quote.json")),
        ("POST", "/quote/multiQuote") => ok(crate::fixture!("samco", "multi_quote.json")),
        ("POST", "/marketDepth") => ok(crate::fixture!("samco", "market_depth.json")),
        ("GET", "/history/candleData") | ("GET", "/history/indexCandleData") => {
            ok(crate::fixture!("samco", "candles_daily.json"))
        }
        ("GET", "/intraday/candleData") => ok(crate::fixture!("samco", "candles_intraday.json")),
        ("GET", "/intraday/indexCandleData") => {
            // Live Samco answers this endpoint under the generic key.
            ok(crate::fixture!("samco", "candles_intraday.json"))
        }
        ("GET", "/doc/ScripMaster.csv") => text(crate::fixture!("samco", "scrip_master.csv")),
        _ => with_status(StatusCode::NOT_FOUND, json!({"status":"Failure"})),
    }
}

#[tokio::test]
async fn sign_in_with_key_and_secret() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let r = b.authenticate(creds("KEY", Some("SECRET"))).await.unwrap();
    assert_eq!(r.auth_token, "<SESSION_TOKEN>");
    assert_eq!(r.user_id, "<USER_ID>");
    assert!(r.feed_token.is_none());
    let call = &fake.calls("/session/token")[0];
    assert_eq!(call.json(), json!({"apiKey":"KEY","apiSecret":"SECRET"}));
    assert_eq!(call.header("content-type"), "application/json");
}

#[tokio::test]
async fn sign_in_errors_name_the_fix() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let e = b.authenticate(creds("BAD", Some("S"))).await.unwrap_err();
    assert!(e.client_message().contains("Invalid API key"));
    assert!(e.client_message().contains("Profile, Broker Configuration"));
    let e = b
        .authenticate(creds("KEY", Some("WRONG")))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Reveal Secret"));
    let e = b
        .authenticate(creds("BLOCKED", Some("S")))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Static IPs"));
    // Missing credentials never reach Samco.
    let n = fake.calls("/session/token").len();
    assert!(b.authenticate(creds("", Some("S"))).await.is_err());
    assert!(b.authenticate(creds("KEY", None)).await.is_err());
    assert_eq!(fake.calls("/session/token").len(), n);
}

#[tokio::test]
async fn ip_status_helper() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let s = b.ip_status(&auth()).await.unwrap();
    assert!(s.matches);
    assert_eq!(s.to_json()["dashboard_url"], samco::DASHBOARD_URL);
    assert_eq!(
        fake.calls("/ip/whoami")[0].header("x-session-token"),
        "<SESSION_TOKEN>"
    );
    let refusing =
        Fake::start(|_r: &Req| ok(json!({"status":"Failure","statusMessage":"Session expired"})))
            .await;
    let e = broker(&refusing).ip_status(&auth()).await.unwrap_err();
    assert_eq!(e.client_message(), "Session expired");
}

fn order(symbol: &str, ex: &str, side: &str, pricetype: &str, price: f64) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: ex.into(),
        side: side.into(),
        quantity: 75,
        price,
        order_type: pricetype.into(),
        product: "NRML".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[tokio::test]
async fn market_order_is_placed_as_protected_limit() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let r = b
        .place_order(&auth(), &order("SBIN", "NSE", "BUY", "MARKET", 0.0))
        .await
        .unwrap();
    assert_eq!(r.order_id, "261003000000201");
    // LTP came from getQuote, without an exchange parameter for NSE.
    let q = &fake.calls("/quote/getQuote")[0];
    assert_eq!(q.param("symbolName").as_deref(), Some("SBIN-EQ"));
    assert_eq!(q.param("exchange"), None);
    let body = fake.calls("/order/placeOrder")[0].json();
    assert_eq!(body["orderType"], "L");
    assert_eq!(body["price"], "816.45");
    assert_eq!(body["marketProtection"], "0.5");
    assert_eq!(body["quantity"], "75");
    assert_eq!(
        fake.calls("/order/placeOrder")[0].header("x-session-token"),
        "<SESSION_TOKEN>"
    );
}

#[tokio::test]
async fn limit_order_modify_and_cancel() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    b.place_order(
        &auth(),
        &order("NIFTY28OCT2624600CE", "NFO", "SELL", "LIMIT", 101.5),
    )
    .await
    .unwrap();
    let body = fake.calls("/order/placeOrder")[0].json();
    assert_eq!(
        (
            body["symbolName"].as_str(),
            body["exchange"].as_str(),
            body["price"].as_str()
        ),
        (Some("NIFTY26OCT24600CE"), Some("NFO"), Some("101.5"))
    );
    assert!(fake.calls("/quote/getQuote").is_empty());

    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "CNC".into(),
        pricetype: "LIMIT".into(),
        quantity: 10,
        price: 811.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("261003000000101", &m, &master()).unwrap();
    let r = b.modify_order(&auth(), &rm).await.unwrap();
    assert_eq!(r.order_id, "261003000000101");
    let put = &fake.calls("/order/modifyOrder/261003000000101")[0];
    assert_eq!(put.method, Method::PUT);
    assert_eq!(
        put.json(),
        json!({"orderType":"L","quantity":"10","orderValidity":"DAY","price":"811.0"})
    );

    b.cancel_order(&auth(), "261003000000101").await.unwrap();
    let e = b
        .cancel_order(&auth(), "261003000000103")
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), "Samco: Order already cancelled");
}

#[tokio::test]
async fn refused_order_reports_samco_reason() {
    let fake = Fake::start(|r: &Req| match r.path.as_str() {
        "/order/placeOrder" => {
            ok(json!({"status":"Failure","statusMessage":"Insufficient margin"}))
        }
        _ => samco_routes(r),
    })
    .await;
    let e = broker(&fake)
        .place_order(&auth(), &order("SBIN", "NSE", "BUY", "LIMIT", 800.0))
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), "Samco: Insufficient margin");
}

#[tokio::test]
async fn cancel_all_touches_open_and_trigger_pending() {
    let fake = Fake::start(samco_routes).await;
    let r = broker(&fake).cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(r.cancelled, vec!["261003000000101"]);
    assert_eq!(r.failed, vec!["261003000000103"]);
    assert_eq!(fake.calls("/order/cancelOrder").len(), 2);
}

#[tokio::test]
async fn close_all_merges_day_and_net() {
    let fake = Fake::start(samco_routes).await;
    let r = broker(&fake).close_all_positions(&auth()).await.unwrap();
    // DAY: SBIN +10 MIS, NIFTY CE -75 NRML; NET adds CRUDEOIL +100
    // (NIFTY CE is deduplicated, DAY wins; SBIN CNC is flat).
    assert_eq!(r.placed.len(), 3, "{:?}", r.failed);
    let bodies: Vec<Value> = fake
        .calls("/order/placeOrder")
        .iter()
        .map(|c| c.json())
        .collect();
    let find = |s: &str| {
        bodies
            .iter()
            .find(|b| b["symbolName"] == s)
            .unwrap()
            .clone()
    };
    let sbin = find("SBIN-EQ");
    assert_eq!(
        (sbin["transactionType"].as_str(), sbin["quantity"].as_str()),
        (Some("SELL"), Some("10"))
    );
    assert_eq!(sbin["productType"], "MIS");
    let opt = find("NIFTY26OCT24600CE");
    assert_eq!(
        (opt["transactionType"].as_str(), opt["quantity"].as_str()),
        (Some("BUY"), Some("75"))
    );
    let crude = find("CRUDEOIL26OCTFUT");
    assert_eq!(
        (
            crude["exchange"].as_str(),
            crude["transactionType"].as_str()
        ),
        (Some("MCX"), Some("SELL"))
    );
    assert_eq!(fake.calls("/position/getPositions").len(), 2);
}

#[tokio::test]
async fn close_all_refuses_when_a_book_is_unreadable() {
    let fake = Fake::start(|r: &Req| {
        if r.path == "/position/getPositions" && r.param("positionType").as_deref() == Some("NET") {
            ok(json!({"status":"Failure","statusMessage":"Service unavailable"}))
        } else {
            samco_routes(r)
        }
    })
    .await;
    let e = broker(&fake)
        .close_all_positions(&auth())
        .await
        .unwrap_err();
    assert!(e
        .client_message()
        .contains("Could not read the NET position book"));
    assert!(fake.calls("/order/placeOrder").is_empty());
}

#[tokio::test]
async fn books_funds_and_open_position() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let o = b.get_order_book(&auth()).await.unwrap();
    assert_eq!(o.len(), 4);
    assert_eq!(
        (o[2].symbol.as_str(), o[2].exchange.as_str()),
        ("CRUDEOIL20OCT26FUT", "MCX")
    );
    let t = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(t[0].symbol, "NIFTY28OCT2624600CE");
    let p = b.get_positions(&auth()).await.unwrap();
    assert_eq!(p[1].quantity, -75);
    assert_eq!(
        fake.calls("/position/getPositions")[0]
            .param("positionType")
            .as_deref(),
        Some("DAY")
    );
    let h = b.get_holdings(&auth()).await.unwrap();
    assert_eq!(h[0].pnl_percentage, 10.0);
    let f = b.get_funds(&auth()).await.unwrap();
    assert_eq!(f.available_cash, 100250.46);
    let q = b
        .get_open_position(&auth(), "NIFTY28OCT2624600CE", Exchange::Nfo, Product::Nrml)
        .await
        .unwrap();
    assert_eq!(q, -75);
    let flat = b
        .get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Cnc)
        .await
        .unwrap();
    assert_eq!(flat, 0);
}

#[tokio::test]
async fn expired_session_is_an_auth_error() {
    let fake =
        Fake::start(|_r: &Req| with_status(StatusCode::UNAUTHORIZED, json!({"status":"Failure"})))
            .await;
    let e = broker(&fake).get_funds(&auth()).await.unwrap_err();
    assert!(e.client_message().contains("session has expired"));
}

#[tokio::test]
async fn span_margin_for_derivatives_only() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let leg = |ex: &str, sym: &str| MarginLeg {
        key: QuoteKey::new(ex, sym),
        action: Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price: 0.0,
        trigger_price: 0.0,
    };
    let m = b
        .calculate_margin(
            &auth(),
            &[leg("NFO", "NIFTY28OCT2624600CE"), leg("NSE", "SBIN")],
        )
        .await
        .unwrap();
    assert_eq!(m.total_margin_required, 152300.25);
    let req = fake.calls("/spanMargin")[0].json();
    assert_eq!(req["request"].as_array().unwrap().len(), 1);
    assert_eq!(req["request"][0]["tradingSymbol"], "NIFTY26OCT24600CE");
    let e = b
        .calculate_margin(&auth(), &[leg("NSE", "SBIN")])
        .await
        .unwrap_err();
    assert!(e.client_message().contains("No valid positions"));
}

#[tokio::test]
async fn quotes_index_quotes_and_depth() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let q = b
        .get_quote(&auth(), &QuoteKey::new("MCX", "CRUDEOIL20OCT26FUT"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 812.4);
    // MCX derivatives are quoted under Samco's MFO.
    assert_eq!(
        fake.calls("/quote/getQuote")[0]
            .param("exchange")
            .as_deref(),
        Some("MFO")
    );
    let n = b
        .get_quote(&auth(), &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!((n.ltp, n.close), (24612.3, 24450.0));
    assert_eq!(
        fake.calls("/quote/indexQuote")[0]
            .param("indexName")
            .as_deref(),
        Some("NIFTY 50")
    );
    // The index quote taught the feed NIFTY's listing id.
    assert_eq!(
        b.listing_ids().lock().get("NSE_INDEX", "NIFTY").as_deref(),
        Some("-23")
    );
    assert_eq!(
        b.index_listing_id(&auth(), &QuoteKey::new("NSE_INDEX", "NIFTY"))
            .await
            .unwrap(),
        "-23"
    );

    let dp = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((dp.bids.len(), dp.asks.len()), (5, 5));
    assert_eq!(
        (dp.bids[0].price, dp.total_buy_qty, dp.ltp),
        (812.35, 120000, 812.4)
    );
    let body = fake.calls("/marketDepth")[0].json();
    assert_eq!(body, json!({"symbolName":"SBIN-EQ"}));
    let idx = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!((idx.ltp, idx.bids[0].price), (24612.3, 0.0));
    let e = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "NOPE"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("NOPE"));
}

#[tokio::test]
async fn multiquotes_batch_and_keep_request_order() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let mut keys = vec![
        QuoteKey::new("NSE_INDEX", "NIFTY"),
        QuoteKey::new("NFO", "NIFTY28OCT2624600CE"),
        QuoteKey::new("NSE", "NOPE"),
    ];
    for _ in 0..30 {
        keys.push(QuoteKey::new("NSE", "SBIN"));
    }
    let out = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(out.len(), keys.len());
    assert_eq!(out[0].data.as_ref().unwrap().ltp, 24612.3);
    assert_eq!(out[1].data.as_ref().unwrap().ltp, 98.1);
    assert_eq!(
        out[2].error.as_deref(),
        Some("Could not resolve broker symbol")
    );
    assert!(out[3..]
        .iter()
        .all(|r| r.data.as_ref().unwrap().ltp == 812.4));
    // 31 resolvable regular symbols -> batches of 25 and 6.
    let calls = fake.calls("/quote/multiQuote");
    assert_eq!(calls.len(), 2);
    let first = calls[0].json();
    assert_eq!(first["NFO"], json!(["NIFTY26OCT24600CE"]));
    assert_eq!(first["NSE"].as_array().unwrap().len(), 24);
    assert_eq!(calls[1].json()["NSE"].as_array().unwrap().len(), 6);
}

#[tokio::test]
async fn data_calls_retry_then_succeed() {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let fake = Fake::start(move |r: &Req| {
        if r.path == "/quote/getQuote" && h.fetch_add(1, Ordering::SeqCst) < 2 {
            with_status(StatusCode::TOO_MANY_REQUESTS, "slow down")
        } else {
            samco_routes(r)
        }
    })
    .await;
    let q = broker(&fake)
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 812.4);
    assert_eq!(hits.load(Ordering::SeqCst), 3);

    let forbidden = Fake::start(|_r: &Req| with_status(StatusCode::FORBIDDEN, "no")).await;
    let e = broker(&forbidden)
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("session has expired"));
    let down = Fake::start(|_r: &Req| {
        with_status(StatusCode::INTERNAL_SERVER_ERROR, json!({"msgId":"m1"}))
    })
    .await;
    let e = broker(&down)
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("not responding"));
    assert_eq!(down.calls("/quote/getQuote").len(), 4);
}

#[tokio::test]
async fn daily_history_stops_at_yesterday_when_range_ends_today() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let req = HistoryRequest {
        key: QuoteKey::new("NFO", "NIFTY28OCT2624600CE"),
        interval: "D".into(),
        start: d(2026, 9, 1),
        end: d(2026, 10, 3),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 2);
    let call = &fake.calls("/history/candleData")[0];
    assert_eq!(
        call.param("symbolName").as_deref(),
        Some("NIFTY26OCT24600CE")
    );
    assert_eq!(call.param("fromDate").as_deref(), Some("2026-09-01"));
    assert_eq!(call.param("toDate").as_deref(), Some("2026-10-02"));
    assert_eq!(call.param("exchange").as_deref(), Some("NFO"));

    let idx = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "D".into(),
        start: d(2026, 9, 1),
        end: d(2026, 9, 30),
    };
    b.get_history(&auth(), &idx).await.unwrap();
    let call = &fake.calls("/history/indexCandleData")[0];
    assert_eq!(call.param("indexName").as_deref(), Some("NIFTY 50"));
    assert_eq!(call.param("toDate").as_deref(), Some("2026-09-30"));
}

#[tokio::test]
async fn intraday_history_and_interval_parameter() {
    let fake = Fake::start(samco_routes).await;
    let b = broker(&fake);
    let mut req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: d(2026, 10, 1),
        end: d(2026, 10, 1),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert!(c[0].timestamp < c[1].timestamp);
    let call = &fake.calls("/intraday/candleData")[0];
    assert_eq!(
        call.param("fromDate").as_deref(),
        Some("2026-10-01 00:00:00")
    );
    assert_eq!(call.param("toDate").as_deref(), Some("2026-10-01 23:59:59"));
    assert_eq!(call.param("interval").as_deref(), Some("5"));
    assert_eq!(call.param("exchange"), None);
    req.interval = "1m".into();
    b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(
        fake.calls("/intraday/candleData")[1].param("interval"),
        None
    );
    req.key = QuoteKey::new("NSE_INDEX", "NIFTY");
    req.interval = "1h".into();
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 2, "the generic intradayCandleData key is accepted");
    req.interval = "3m".into();
    assert!(b.get_history(&auth(), &req).await.is_err());
}

#[tokio::test]
async fn scrip_master_download() {
    let fake = Fake::start(samco_routes).await;
    let rows = broker(&fake)
        .download_master_contract(&auth())
        .await
        .unwrap();
    assert_eq!(rows.len(), 13 + 68);
    assert!(rows
        .iter()
        .any(|r| r.symbol == "CRUDEOIL20OCT26FUT" && r.exchange == "MCX" && r.brexchange == "MFO"));
    assert!(rows
        .iter()
        .any(|r| r.symbol == "BANKNIFTY" && r.exchange == "NSE_INDEX"));
    let missing = Fake::start(|_r: &Req| with_status(StatusCode::NOT_FOUND, "")).await;
    assert!(broker(&missing)
        .download_master_contract(&auth())
        .await
        .is_err());
}

#[tokio::test]
async fn feed_streams_from_a_fake_socket() {
    use futures_util::{SinkExt, StreamExt};
    let ws = FakeWs::start(|mut s: ServerWs| async move {
        // The subscribe frame arrives newline-terminated; answer with a tick.
        if let Some(t) = next_text(&mut s).await {
            assert!(t.ends_with('\n'));
            send_text(
                &mut s,
                json!({"sym":"3045_NSE","ltp":"812.40","c":"800","streaming_type":"quote"})
                    .to_string(),
            )
            .await;
        }
        let _ = next_text(&mut s).await;
    })
    .await;
    let fake = Fake::start(samco_routes).await;
    let b = SamcoBroker::with_urls(master(), &fake.base, "", &ws.url);
    let mut feed = b.create_feed(&auth()).unwrap();
    let req = feed.ws_request().unwrap();
    let (mut sock, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let sub = FeedSubscription {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        token: "3045_NSE".into(),
        brsymbol: "SBIN-EQ".into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Quote,
        depth: 5,
    };
    for f in feed.subscribe_frames(&[sub]) {
        sock.send(f).await.unwrap();
    }
    let msg = tokio::time::timeout(Duration::from_secs(5), sock.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ev = feed.parse(&msg);
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("tick expected")
    };
    assert_eq!((t.symbol.as_str(), t.ltp, t.change), ("SBIN", 812.4, 12.4));
    let hs = ws.handshakes.lock().clone();
    assert_eq!(hs[0].1["x-session-token"], "<SESSION_TOKEN>");
    let _ = sock.close(None).await;
}
