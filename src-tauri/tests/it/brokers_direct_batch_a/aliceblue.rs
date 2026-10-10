//! AliceBlue against a local fake broker: the V2 vendor sign-in (checksum
//! verified by the fake), order bodies, books, funds, socket-backed quotes,
//! multiquotes and depth over a fake Noren socket, history with
//! resampling, the master contract download, and both live feeds through
//! the loopback relay.

#![allow(unused_imports)]

use super::support::*;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::aliceblue::{AliceBlueBroker, Endpoints, QuoteTiming};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const JWT: &str = "header.payload.signature";

fn sha(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn auth() -> AuthToken {
    AuthToken::new(JWT).with_user_id("AB1")
}

fn symbols() -> SymbolResolver {
    let r = SymbolResolver::new();
    let mut nfo = row(
        "NIFTY28OCT2625000CE",
        "NIFTY28OCT2625000CE",
        "NFO",
        "NFO",
        "54957",
        75,
        0.05,
    );
    nfo.instrument_type = "CE".into();
    r.load(vec![
        row("SBIN", "SBIN-EQ", "NSE", "NSE", "3045", 1, 0.05),
        row("INFY", "INFY-EQ", "NSE", "NSE", "1594", 1, 0.05),
        row("NIFTYBEES", "NIFTYBEES-EQ", "NSE", "NSE", "10576", 1, 0.01),
        row("TATAMOTORS", "TATAMOTORS", "BSE", "BSE", "500570", 1, 0.05),
        nfo,
        row("NIFTY", "NIFTY 50", "NSE_INDEX", "NSE", "26000", 1, 0.01),
    ]);
    r
}

fn timing() -> QuoteTiming {
    QuoteTiming {
        connect: Duration::from_secs(3),
        single: Duration::from_millis(600),
        retry_pause: Duration::from_millis(10),
        multi_per_symbol: Duration::from_millis(10),
        multi_floor: Duration::from_millis(500),
        multi_ceiling: Duration::from_secs(2),
        straggler: Duration::from_millis(100),
        poll: Duration::from_millis(10),
        idle: Duration::from_secs(30),
    }
}

fn endpoints(base: &str, ws: &str) -> Endpoints {
    Endpoints {
        base: base.into(),
        auth: base.into(),
        master: format!("{}/master", base),
        ws: vec![ws.into()],
        order_ws: ws.into(),
    }
}

fn broker(fake: &Fake, ws: &str) -> AliceBlueBroker {
    AliceBlueBroker::with_endpoints(symbols(), endpoints(&fake.base, ws))
        .with_quote_timing(timing())
}

/// The fake trading host.
fn rest(req: &Req) -> Response {
    let auth_ok = req.header("authorization") == format!("Bearer {}", JWT);
    match req.path.as_str() {
        "/open-api/od/v1/vendor/getUserDetails" => {
            if req.json()["checkSum"] == sha("AB123ac9sec") {
                ok(json!({"stat":"Ok","userSession":JWT,"clientId":"AB1"}))
            } else {
                ok(json!({"stat":"Not_ok","emsg":"Invalid checksum"}))
            }
        }
        p if p.starts_with("/master/") => {
            let name = p.trim_start_matches("/master/");
            let body = match name {
                "NSE.csv" => crate::fixture!("aliceblue", "NSE.csv"),
                "BSE.csv" => crate::fixture!("aliceblue", "BSE.csv"),
                "NFO.csv" => crate::fixture!("aliceblue", "NFO.csv"),
                "CDS.csv" => crate::fixture!("aliceblue", "CDS.csv"),
                "MCX.csv" => crate::fixture!("aliceblue", "MCX.csv"),
                "BFO.csv" => crate::fixture!("aliceblue", "BFO.csv"),
                "BCD.csv" => crate::fixture!("aliceblue", "BCD.csv"),
                "INDICES.csv" => crate::fixture!("aliceblue", "INDICES.csv"),
                _ => return with_status(StatusCode::NOT_FOUND, "{}"),
            };
            text(body)
        }
        _ if !auth_ok => with_status(StatusCode::UNAUTHORIZED, json!({"status":"Not_Ok"})),
        "/open-api/od/v1/orders/placeorder" => {
            let item = &req.json()[0];
            if item["quantity"] == 0 {
                ok(
                    json!({"status":"Ok","result":[{"status":"Not_Ok","brokerOrderId":"","message":"EC904"}]}),
                )
            } else {
                ok(
                    json!({"status":"Ok","result":[{"brokerOrderId":"25100300000999","status":"Ok"}]}),
                )
            }
        }
        "/open-api/od/v1/orders/modify" => {
            ok(json!({"status":"Ok","result":[{"brokerOrderId":req.json()["brokerOrderId"]}]}))
        }
        "/open-api/od/v1/orders/cancel" => {
            if req.json()["brokerOrderId"] == "BAD" {
                ok(json!({"status":"Not_Ok","message":"EC937"}))
            } else {
                ok(json!({"status":"Ok","result":[{"brokerOrderId":req.json()["brokerOrderId"]}]}))
            }
        }
        "/open-api/od/v1/orders/book" => ok(crate::fixture!("aliceblue", "order_book.json")),
        "/open-api/od/v1/orders/trades" => ok(crate::fixture!("aliceblue", "trade_book.json")),
        "/open-api/od/v1/positions" => ok(crate::fixture!("aliceblue", "positions.json")),
        "/open-api/od/v1/holdings/CNC" => ok(crate::fixture!("aliceblue", "holdings.json")),
        "/open-api/od/v1/limits/" => ok(crate::fixture!("aliceblue", "limits.json")),
        "/open-api/od/v1/profile/invalidateWsSess" => ok(json!({"status":"Ok"})),
        "/open-api/od/v1/profile/createWsSess" => ok(json!({"status":"Ok","result":[]})),
        "/open-api/order-notify/ws/createWsToken" => {
            ok(json!({"status":"Ok","result":[{"orderToken":"ot-1"}]}))
        }
        "/open-api/od/ChartAPIService/api/chart/history" => {
            if req.json()["resolution"] == "D" {
                ok(crate::fixture!("aliceblue", "history_day.json"))
            } else {
                ok(crate::fixture!("aliceblue", "history_1m.json"))
            }
        }
        _ => with_status(StatusCode::NOT_FOUND, json!({"status":"Not_Ok"})),
    }
}

/// A fake Noren socket: answers the login, then a `tk`/`dk` snapshot for
/// every subscribed key except token 9 (never answers).
async fn noren(mut ws: ServerWs, frames: Arc<Mutex<Vec<Value>>>) {
    while let Some(t) = next_text(&mut ws).await {
        let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
        frames.lock().push(v.clone());
        match v["t"].as_str() {
            Some("c") => send_text(&mut ws, json!({"t":"ck","s":"OK"}).to_string()).await,
            Some(kind @ ("t" | "d")) => {
                for key in v["k"].as_str().unwrap_or("").split('#') {
                    let (e, tk) = key.split_once('|').unwrap_or(("", ""));
                    if tk == "9" {
                        continue;
                    }
                    let lp = format!("{}.5", tk.len() * 100);
                    let frame = if kind == "t" {
                        json!({"t":"tk","e":e,"tk":tk,"lp":lp,"o":"10","h":"20","l":"5",
                            "c":"9","v":"1000","bp1":"1.1","sp1":"1.2","bq1":"3","sq1":"4","oi":"77"})
                    } else {
                        json!({"t":"dk","e":e,"tk":tk,"lp":lp,"bp1":"99.5","bq1":"10","sp1":"100",
                            "sq1":"20","tbq":"500","tsq":"600","c":"98","ltq":"2"})
                    };
                    send_text(&mut ws, frame.to_string()).await;
                }
            }
            _ => {}
        }
    }
}

async fn noren_ws() -> (FakeWs, Arc<Mutex<Vec<Value>>>) {
    let frames: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let f = frames.clone();
    let ws = FakeWs::start(move |ws| noren(ws, f.clone())).await;
    (ws, frames)
}

#[tokio::test]
async fn login_checksum_and_refusal() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let ok = b
        .authenticate(BrokerCredentials {
            api_key: "APPCODE".into(),
            api_secret: Some("sec".into()),
            auth_code: Some("AB123:ac9".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ok.auth_token, JWT);
    assert_eq!(ok.user_id, "AB1");
    assert!(ok.feed_token.is_none());
    let req = &fake.calls("getUserDetails")[0];
    assert_eq!(req.json(), json!({"checkSum": sha("AB123ac9sec")}));

    let err = b
        .authenticate(BrokerCredentials {
            api_key: "APPCODE".into(),
            api_secret: Some("wrong".into()),
            auth_code: Some("AB123:ac9".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(
        err.client_message().contains("Invalid checksum"),
        "{}",
        err.client_message()
    );
    let err = b
        .authenticate(BrokerCredentials {
            api_key: "APPCODE".into(),
            api_secret: None,
            auth_code: Some("AB123:ac9".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(err.client_message().contains("API secret"));
}

fn order(symbol: &str, exchange: &str, qty: i32, pricetype: &str) -> ResolvedOrder {
    ResolvedOrder::resolve(
        &OrderRequest {
            symbol: symbol.into(),
            exchange: exchange.into(),
            side: "BUY".into(),
            quantity: qty,
            price: 0.0,
            order_type: pricetype.into(),
            product: "MIS".into(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        },
        &symbols(),
    )
    .unwrap()
}

#[tokio::test]
async fn orders_modify_cancel() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let r = b
        .place_order(&auth(), &order("SBIN", "NSE", 10, "MARKET"))
        .await
        .unwrap();
    assert_eq!(r.order_id, "25100300000999");
    let call = &fake.calls("placeorder")[0];
    assert_eq!(call.header("authorization"), format!("Bearer {}", JWT));
    let body = call.json();
    assert!(body.is_array());
    assert_eq!(body[0]["instrumentId"], "3045");
    assert_eq!(body[0]["product"], "INTRADAY");
    assert_eq!(body[0]["orderType"], "MARKET");
    assert_eq!(body[0]["orderTag"], "openalgo");

    let err = b
        .place_order(&auth(), &order("SBIN", "NSE", 0, "MARKET"))
        .await
        .unwrap_err();
    assert!(
        err.client_message().contains("EC904"),
        "{}",
        err.client_message()
    );

    let m = ResolvedModify::resolve(
        "25100300000103",
        &ModifyOrderRequest {
            symbol: "INFY".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "CNC".into(),
            pricetype: "LIMIT".into(),
            quantity: 5,
            price: 1499.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        },
        &symbols(),
    )
    .unwrap();
    let r = b.modify_order(&auth(), &m).await.unwrap();
    assert_eq!(r.order_id, "25100300000103");
    let mb = fake.calls("orders/modify")[0].json();
    assert_eq!(mb["price"], "1499.0");
    assert_eq!(mb["orderType"], "LIMIT");

    assert_eq!(b.cancel_order(&auth(), "X1").await.unwrap().order_id, "X1");
    let err = b.cancel_order(&auth(), "BAD").await.unwrap_err();
    assert!(err.client_message().contains("Failed to cancel all orders"));
}

#[tokio::test]
async fn cancel_all_and_close_all() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(r.cancelled, ["25100300000102", "25100300000103"]);
    assert!(r.failed.is_empty());

    let r = b.close_all_positions(&auth()).await.unwrap();
    assert_eq!(r.placed.len(), 2, "{:?}", r.failed);
    let bodies: Vec<Value> = fake
        .calls("placeorder")
        .iter()
        .map(|c| c.json()[0].clone())
        .collect();
    assert_eq!(bodies[0]["transactionType"], "SELL");
    assert_eq!(bodies[0]["quantity"], 10);
    assert_eq!(bodies[0]["product"], "INTRADAY");
    assert_eq!(bodies[1]["transactionType"], "BUY");
    assert_eq!(bodies[1]["quantity"], 75);
    assert_eq!(bodies[1]["exchange"], "NFO");
    assert_eq!(bodies[1]["product"], "NRML");

    let q = b
        .get_open_position(&auth(), "NIFTY28OCT2625000CE", Exchange::Nfo, Product::Nrml)
        .await
        .unwrap();
    assert_eq!(q, -75);
    let q = b
        .get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Cnc)
        .await
        .unwrap();
    assert_eq!(q, 0);
}

#[tokio::test]
async fn books_and_funds() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let o = b.get_order_book(&auth()).await.unwrap();
    assert_eq!(o.len(), 4);
    assert_eq!(o[1].symbol, "NIFTY28OCT2625000CE");
    assert!(o.iter().all(|x| x.status == x.status.to_lowercase()));
    let t = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(t[0].symbol, "SBIN");
    let p = b.get_positions(&auth()).await.unwrap();
    assert_eq!(p.len(), 3);
    let h = b.get_holdings(&auth()).await.unwrap();
    assert_eq!(h[0].symbol, "NIFTYBEES");
    let f = b.get_funds(&auth()).await.unwrap();
    assert_eq!(f.available_cash, 125000.46);
    assert_eq!(f.m2m_realized, 50.0);

    // An expired session is a sign-in problem, not an empty book.
    let err = b
        .get_order_book(&AuthToken::new("stale"))
        .await
        .unwrap_err();
    assert!(err.client_message().contains("session has expired"));
}

#[tokio::test]
async fn empty_books_from_broker_codes() {
    let fake = Fake::start(|req: &Req| match req.path.as_str() {
        "/open-api/od/v1/orders/book" => ok(json!({"status":"Not_Ok","message":"EC916"})),
        "/open-api/od/v1/positions" => ok(json!({"status":"Not_Ok","message":"EC919"})),
        "/open-api/od/v1/holdings/CNC" => ok(json!({"status":"Not_Ok","message":"EC003"})),
        _ => ok(json!({"status":"Not_Ok","message":"EC926"})),
    })
    .await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    assert!(b.get_order_book(&auth()).await.unwrap().is_empty());
    assert!(b.get_trade_book(&auth()).await.unwrap().is_empty());
    assert!(b.get_positions(&auth()).await.unwrap().is_empty());
    // The smart-order read refuses a failed position book.
    let err = b
        .get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Mis)
        .await
        .unwrap_err();
    assert!(err.client_message().contains("EC919"));
    let err = b.get_holdings(&auth()).await.unwrap_err();
    assert!(err.client_message().contains("An error occurred"));
    // BR-02: close all on a book that failed to load (EC919) is an error,
    // not "no positions" (the web closes nothing and reports success).
    let err = b.close_all_positions(&auth()).await.unwrap_err();
    assert!(
        err.client_message().contains("no position was closed"),
        "{}",
        err.client_message()
    );
    // AliceBlue's own "no orders" (EC916) is an empty book for cancel all.
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    assert!(r.cancelled.is_empty() && r.failed.is_empty());
}

/// BR-02: cancel all and close all never read a book they could not load
/// as "nothing to do": a failed read (EC915/EC919, their sentences), an
/// error page, or an `Ok` without its rows is an error with no cancel or
/// exit sent; only AliceBlue's own empty answers (EC916/EC920) or an empty
/// list are empty.
#[tokio::test]
async fn cancel_all_and_close_all_refuse_an_unread_book() {
    for (status, body, empty) in [
        (200u16, json!({"status":"Not_Ok","message":"EC915"}).to_string(), false),
        (
            200,
            json!({"status":"Not_Ok","message":"Failed to retrieve the order book."}).to_string(),
            false,
        ),
        (500, "<html>Bad gateway</html>".to_string(), false),
        (200, json!({"status":"Ok"}).to_string(), false),
        (200, json!({"status":"Not_Ok","message":"EC916"}).to_string(), true),
        (200, json!({"status":"Ok","result":[]}).to_string(), true),
    ] {
        let body2 = body.clone();
        let fake = Fake::start(move |req: &Req| match req.path.as_str() {
            "/open-api/od/v1/orders/book" | "/open-api/od/v1/positions" => {
                // The order-book reply, reused for the positions book with
                // the position codes.
                let b = if req.path.ends_with("positions") {
                    body2.replace("EC915", "EC919").replace("EC916", "EC920").replace(
                        "Failed to retrieve the order book.",
                        "Failed to retrieve the position book.",
                    )
                } else {
                    body2.clone()
                };
                with_status(StatusCode::from_u16(status).unwrap(), b)
            }
            _ => rest(req),
        })
        .await;
        let b = broker(&fake, "ws://127.0.0.1:9");
        let cancel = b.cancel_all_orders(&auth()).await;
        let close = b.close_all_positions(&auth()).await;
        if empty {
            let c = cancel.unwrap();
            assert!(c.cancelled.is_empty() && c.failed.is_empty(), "{}", body);
            assert!(close.unwrap().placed.is_empty(), "{}", body);
        } else {
            let e = cancel.unwrap_err().client_message();
            assert!(e.contains("no order was cancelled"), "{}: {}", body, e);
            let e = close.unwrap_err().client_message();
            assert!(e.contains("no position was closed"), "{}: {}", body, e);
        }
        assert!(fake.calls("orders/cancel").is_empty(), "{}", body);
        assert!(fake.calls("placeorder").is_empty(), "{}", body);
    }
}

#[tokio::test]
async fn quotes_over_one_pooled_socket() {
    let fake = Fake::start(rest).await;
    let (ws, frames) = noren_ws().await;
    let b = broker(&fake, &ws.url);
    let q = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 400.5);
    assert_eq!(q.close, 9.0);
    assert_eq!(q.volume, 1000);
    assert_eq!((q.bid, q.ask, q.oi), (1.1, 1.2, 77));
    // The index token goes out as NSE|26000.
    let q = b
        .get_quote(&auth(), &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 500.5);
    // One session prep and one socket for both quotes.
    assert_eq!(fake.calls("createWsSess").len(), 1);
    assert_eq!(fake.calls("invalidateWsSess").len(), 1);
    assert_eq!(ws.handshakes.lock().len(), 1);
    let sent = frames.lock().clone();
    assert_eq!(sent[0]["t"], "c");
    assert_eq!(sent[0]["actid"], "AB1_API");
    assert!(sent.contains(&json!({"t":"t","k":"NSE|3045"})));
    assert!(sent.contains(&json!({"t":"t","k":"NSE|26000"})));
    // Released after use: unsubscribed, nothing tracked.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(frames.lock().contains(&json!({"t":"u","k":"NSE|3045"})));
    b.close_quote_socket();
}

#[tokio::test]
async fn multiquotes_and_depth() {
    let fake = Fake::start(rest).await;
    let (ws, frames) = noren_ws().await;
    let b = broker(&fake, &ws.url);
    let keys = [
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NFO", "NIFTY28OCT2625000CE"),
    ];
    let r = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].data.as_ref().unwrap().ltp, 400.5);
    assert_eq!(r[1].error.as_deref(), Some("Could not resolve token"));
    assert_eq!(r[2].symbol, "NIFTY28OCT2625000CE");
    assert_eq!(r[2].data.as_ref().unwrap().ltp, 500.5);
    assert!(frames
        .lock()
        .contains(&json!({"t":"t","k":"NSE|3045#NFO|54957"})));

    let d = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE", "INFY"))
        .await
        .unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.bids[0].price, 99.5);
    assert_eq!(d.asks[0].quantity, 20);
    assert_eq!(d.total_sell_qty, 600);
    assert_eq!(d.prev_close, 98.0);
    assert!(frames.lock().contains(&json!({"t":"d","k":"NSE|1594"})));
}

#[tokio::test]
async fn quote_without_data_is_a_trader_error() {
    let fake = Fake::start(rest).await;
    let (ws, _frames) = noren_ws().await;
    let r = SymbolResolver::new();
    r.load(vec![row("DEAD", "DEAD-EQ", "NSE", "NSE", "9", 1, 0.05)]);
    let b = AliceBlueBroker::with_endpoints(r, endpoints(&fake.base, &ws.url)).with_quote_timing(
        QuoteTiming {
            single: Duration::from_millis(100),
            ..timing()
        },
    );
    let err = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "DEAD"))
        .await
        .unwrap_err();
    assert!(err.client_message().contains("sent no price for DEAD"));
    // The retry used a fresh socket.
    assert_eq!(ws.handshakes.lock().len(), 2);
}

#[tokio::test]
async fn refused_socket_session_is_a_sign_in_error() {
    let fake = Fake::start(|req: &Req| match req.path.as_str() {
        "/open-api/od/v1/profile/createWsSess" => {
            with_status(StatusCode::UNAUTHORIZED, json!({"status":"Not_Ok"}))
        }
        _ => ok(json!({"status":"Ok"})),
    })
    .await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let err = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert!(err.client_message().contains("Log in to AliceBlue again"));
}

#[tokio::test]
async fn history_requests_and_resampling() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: d(2026, 10, 1),
        end: d(2026, 10, 1),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].volume, 2200);
    let body = fake.calls("chart/history")[0].json();
    assert_eq!(body["token"], "3045");
    assert_eq!(body["exchange"], "NSE");
    assert_eq!(body["resolution"], "1");
    assert_eq!(body["from"], "1790826300000");

    let idx = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "D".into(),
        start: d(2026, 9, 30),
        end: d(2026, 10, 1),
    };
    let c = b.get_history(&auth(), &idx).await.unwrap();
    assert_eq!(c.len(), 2);
    let body = fake.calls("chart/history")[1].json();
    assert_eq!(body["exchange"], "NSE::index");
    assert_eq!(body["resolution"], "D");

    let bad = HistoryRequest {
        interval: "2m".into(),
        ..req
    };
    assert!(b.get_history(&auth(), &bad).await.is_err());
}

#[tokio::test]
async fn master_contract_download() {
    let fake = Fake::start(rest).await;
    let b = broker(&fake, "ws://127.0.0.1:9");
    let rows = b.download_master_contract(&auth()).await.unwrap();
    assert_eq!(rows.len(), 23);
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTY28OCT2625000CE" && r.expiry == "28-OCT-26"));
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTY" && r.exchange == "NSE_INDEX"));
    assert_eq!(fake.calls(".csv").len(), 8);

    let missing = Fake::start(|req: &Req| {
        if req.path.ends_with("BCD.csv") {
            with_status(StatusCode::NOT_FOUND, "{}")
        } else {
            text("Exch,Token\n")
        }
    })
    .await;
    let b = broker(&missing, "ws://127.0.0.1:9");
    assert!(b.download_master_contract(&auth()).await.is_err());
}

async fn connect_feed(
    feed: &dyn BrokerFeed,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let req = feed.ws_request().unwrap();
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws
}

async fn next_parsed(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    feed: &mut dyn BrokerFeed,
) -> Vec<FeedEvent> {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let ev = feed.parse(&m);
        if !ev.is_empty() {
            return ev;
        }
    }
}

#[tokio::test]
async fn live_feed_through_the_relay() {
    let fake = Fake::start(rest).await;
    let (ws, frames) = noren_ws().await;
    let b = broker(&fake, &ws.url);
    let mut feed = b.create_feed(&auth()).unwrap();
    let mut conn = connect_feed(feed.as_ref()).await;
    assert_eq!(
        next_parsed(&mut conn, feed.as_mut()).await,
        vec![FeedEvent::AuthOk]
    );
    assert_eq!(fake.calls("createWsSess").len(), 1);
    let sub = FeedSubscription {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        token: "3045".into(),
        brsymbol: "SBIN-EQ".into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Quote,
        depth: 5,
    };
    for f in feed.subscribe_frames(std::slice::from_ref(&sub)) {
        conn.send(f).await.unwrap();
    }
    let ev = next_parsed(&mut conn, feed.as_mut()).await;
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("tick expected, got {:?}", ev)
    };
    assert_eq!((t.symbol.as_str(), t.ltp, t.close), ("SBIN", 400.5, 9.0));
    assert!(frames.lock().iter().any(|f| f["t"] == "c"));
}

#[tokio::test]
async fn order_feed_through_the_relay() {
    let fake = Fake::start(rest).await;
    let first: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let seen = first.clone();
    let ws = FakeWs::start(move |mut ws| {
        let seen = seen.clone();
        async move {
            if let Some(t) = next_text(&mut ws).await {
                *seen.lock() = serde_json::from_str(&t).ok();
                let om = json!({"t":"om","norenordno":"25100300000101","tsym":"SBIN-EQ",
                    "exch":"NSE","trantype":"S","qty":"10","prc":"812","prctyp":"L",
                    "pcode":"MIS","status":"OPEN","fillshares":"0"});
                send_text(&mut ws, om.to_string()).await;
            }
            while next_text(&mut ws).await.is_some() {}
        }
    })
    .await;
    let b = broker(&fake, &ws.url);
    // Through the trait hook the broker runtime uses.
    let OrderFeed::Socket(mut feed) = Broker::create_order_feed(&b, &auth()).unwrap() else {
        panic!("AliceBlue order updates come from a socket")
    };
    let mut conn = connect_feed(feed.as_ref()).await;
    assert_eq!(
        next_parsed(&mut conn, feed.as_mut()).await,
        vec![FeedEvent::AuthOk]
    );
    let ev = next_parsed(&mut conn, feed.as_mut()).await;
    let FeedEvent::OrderUpdate(u) = &ev[0] else {
        panic!("order update expected")
    };
    assert_eq!(u.symbol, "SBIN");
    assert_eq!(u.action, "SELL");
    assert_eq!(u.pricetype, "LIMIT");
    assert_eq!(u.order_status, "open");
    assert_eq!(u.pending_quantity, 10);
    assert_eq!(
        first.lock().clone(),
        Some(json!({"orderToken":"ot-1","userId":"AB1"}))
    );
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
        let b = AliceBlueBroker::with_endpoints(symbols(), endpoints(&base, &closed_ws()))
            .with_quote_timing(timing());
        clean_err(b.authenticate(sentinel_creds()).await);
        let auth = AuthToken::new(SENTINEL)
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
