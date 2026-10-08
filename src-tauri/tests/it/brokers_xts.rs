//! XTS family adapters against a local fake XTS (ephemeral ports): both
//! logins for every login variant, orders, books, funds, margin, quotes
//! with OI, multiquotes in batches, depth, history windows, the master
//! download, the "Invalid Token" refresh, and the market-data feed end to
//! end (the shared `WebSocketManager` -> loopback relay -> a fake
//! Engine.IO v4 / Socket.IO server, with REST subscriptions).

use axum::body::Bytes;
use axum::extract::{OriginalUri, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::NaiveDate;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::mapping::{Action, PriceType, Product};
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::families::xts::{master_contract, XtsBroker, XtsConfig};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{
    compositedge, fivepaisaxts, jainamxts, rmoney, Broker, BrokerCredentials,
};
use openalgo_desktop_lib::websocket::{FeedConfig, WebSocketManager};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;

const INTERACTIVE: &str = "eyJhbGciOiJIUzI1NiJ9.interactive.sig";
const MARKET: &str = "eyJhbGciOiJIUzI1NiJ9.market.sig";

fn books() -> Value {
    serde_json::from_str(include_str!("../fixtures/brokers/fivepaisaxts/books.json")).unwrap()
}

fn market() -> Value {
    serde_json::from_str(include_str!("../fixtures/brokers/fivepaisaxts/market.json")).unwrap()
}

fn stream(k: &str) -> String {
    let v: Value =
        serde_json::from_str(include_str!("../fixtures/brokers/fivepaisaxts/stream.json")).unwrap();
    v[k].as_str().unwrap().to_string()
}

fn segment_file(seg: &str) -> &'static str {
    match seg {
        "NSECM" => include_str!("../fixtures/brokers/fivepaisaxts/NSECM.txt"),
        "BSECM" => include_str!("../fixtures/brokers/fivepaisaxts/BSECM.txt"),
        "NSEFO" => include_str!("../fixtures/brokers/fivepaisaxts/NSEFO.txt"),
        "NSECD" => include_str!("../fixtures/brokers/fivepaisaxts/NSECD.txt"),
        "MCXFO" => include_str!("../fixtures/brokers/fivepaisaxts/MCXFO.txt"),
        _ => "",
    }
}

fn master() -> SymbolResolver {
    let mut rows = Vec::new();
    for seg in ["NSECM", "BSECM", "NSEFO"] {
        rows.extend(master_contract::parse_segment(seg, segment_file(seg)));
    }
    rows.extend(master_contract::parse_index_list(
        1,
        &market()["indexlist_1"]["result"],
    ));
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<String>>,
    /// Market-data calls with this token answer "Invalid Token".
    expired_feed: Mutex<Option<String>>,
}

type S = State<Arc<Fake>>;

impl Fake {
    fn log(&self, s: String) {
        self.seen.lock().push(s);
    }
    fn lines(&self, prefix: &str) -> Vec<String> {
        self.seen
            .lock()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .cloned()
            .collect()
    }
}

fn auth(h: &HeaderMap) -> String {
    h.get("authorization")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

fn quote_for(code: u64, inst: &Value) -> String {
    let seg = inst["exchangeSegment"].clone();
    let id = inst["exchangeInstrumentID"].clone();
    if code == 1510 {
        return json!({"MessageCode": 1510, "ExchangeSegment": seg, "ExchangeInstrumentID": id, "OpenInterest": 1234500}).to_string();
    }
    let mut q = market()["quote_payload"].clone();
    q["ExchangeSegment"] = seg;
    q["ExchangeInstrumentID"] = id;
    q.to_string()
}

fn md_routes(prefix: &str) -> Router<Arc<Fake>> {
    Router::new()
        .route(
            &format!("{}/auth/login", prefix),
            post(
                |State(s): S, OriginalUri(u): OriginalUri, Json(b): Json<Value>| async move {
                    s.log(format!("mdlogin|{}|{}", u.path(), b));
                    if b["appKey"] == "bad" {
                        return Json(json!({"type": "error", "description": "Invalid appKey"}));
                    }
                    Json(books()["market_login_ok"].clone())
                },
            ),
        )
        .route(
            &format!("{}/instruments/quotes", prefix),
            post(
                |State(s): S, h: HeaderMap, Json(b): Json<Value>| async move {
                    let token = auth(&h);
                    let code = b["xtsMessageCode"].as_u64().unwrap();
                    let n = b["instruments"].as_array().unwrap().len();
                    s.log(format!("quotes|{}|{}|{}", token, code, n));
                    if s.expired_feed.lock().as_deref() == Some(token.as_str()) {
                        return Json(books()["invalid_token"].clone());
                    }
                    let list: Vec<Value> = b["instruments"]
                        .as_array()
                        .unwrap()
                        .iter()
                        // Unknown instrument 999: XTS just leaves it out.
                        .filter(|i| i["exchangeInstrumentID"] != 999)
                        .map(|i| Value::String(quote_for(code, i)))
                        .collect();
                    Json(json!({"type": "success", "result": {"listQuotes": list}}))
                },
            ),
        )
        .route(
            &format!("{}/instruments/ohlc", prefix),
            get(
                |State(s): S, h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    s.log(format!(
                        "ohlc|{}|{}|{}|{}|{}|{}",
                        auth(&h),
                        q["exchangeSegment"],
                        q["exchangeInstrumentID"],
                        q["startTime"],
                        q["endTime"],
                        q["compressionValue"]
                    ));
                    if q["compressionValue"] == "D" {
                        return Json(market()["ohlc_empty"].clone());
                    }
                    Json(market()["ohlc_minute"].clone())
                },
            ),
        )
        .route(
            &format!("{}/instruments/master", prefix),
            post(
                |State(s): S, h: HeaderMap, Json(b): Json<Value>| async move {
                    let seg = b["exchangeSegmentList"][0].as_str().unwrap().to_string();
                    s.log(format!("master|{}|{}", seg, auth(&h)));
                    Json(json!({"type": "success", "result": segment_file(&seg)}))
                },
            ),
        )
        .route(
            &format!("{}/instruments/indexlist", prefix),
            get(
                |State(s): S, Query(q): Query<HashMap<String, String>>| async move {
                    let seg = q["exchangeSegment"].clone();
                    s.log(format!("indexlist|{}", seg));
                    Json(market()[format!("indexlist_{}", seg)].clone())
                },
            ),
        )
        .route(
            &format!("{}/instruments/subscription", prefix),
            post(
                |State(s): S, h: HeaderMap, Json(b): Json<Value>| async move {
                    s.log(format!("subscribe|{}|{}", auth(&h), b));
                    Json(market()["subscription_ok"].clone())
                },
            )
            .put(
                |State(s): S, h: HeaderMap, Json(b): Json<Value>| async move {
                    s.log(format!("unsubscribe|{}|{}", auth(&h), b));
                    Json(market()["unsubscription_ok"].clone())
                },
            ),
        )
}

fn app(state: Arc<Fake>) -> Router {
    Router::new()
        .route(
            "/interactive/user/session",
            post(|State(s): S, Json(b): Json<Value>| async move {
                s.log(format!("session|{}", b));
                if b["secretKey"] == "wrong" {
                    return Json(books()["session_refused"].clone());
                }
                Json(books()["session_ok"].clone())
            }),
        )
        .route(
            "/interactive/orders",
            get(|State(s): S, h: HeaderMap| async move {
                s.log(format!("orderbook|{}", auth(&h)));
                if auth(&h) == "expired" {
                    return Json(books()["invalid_token"].clone());
                }
                Json(books()["orders"].clone())
            })
            .post(
                |State(s): S, h: HeaderMap, Json(b): Json<Value>| async move {
                    s.log(format!("place|{}|{}", auth(&h), b));
                    if b["orderQuantity"] == 7 {
                        return Json(books()["place_rejected"].clone());
                    }
                    Json(books()["place_ok"].clone())
                },
            )
            .put(|State(s): S, h: HeaderMap, body: Bytes| async move {
                s.log(format!(
                    "modify|{}|{}",
                    auth(&h),
                    String::from_utf8_lossy(&body)
                ));
                Json(books()["modify_ok"].clone())
            })
            .delete(|State(s): S, OriginalUri(u): OriginalUri| async move {
                s.log(format!("cancel|{}", u.query().unwrap_or("")));
                Json(books()["cancel_ok"].clone())
            }),
        )
        .route(
            "/interactive/orders/trades",
            get(|| async { Json(books()["trades"].clone()) }),
        )
        .route(
            "/interactive/portfolio/positions",
            get(|State(s): S, OriginalUri(u): OriginalUri| async move {
                s.log(format!("positions|{}", u.query().unwrap_or("")));
                Json(books()["positions"].clone())
            }),
        )
        .route(
            "/interactive/portfolio/holdings",
            get(|| async { Json(books()["holdings"].clone()) }),
        )
        .route(
            "/interactive/user/balance",
            get(|| async { Json(books()["balance"].clone()) }),
        )
        .route(
            "/interactive/orders/margindetails",
            post(|State(s): S, Json(b): Json<Value>| async move {
                s.log(format!("margin|{}", b));
                Json(books()["margin_ok"].clone())
            }),
        )
        .merge(md_routes("/apimarketdata"))
        .merge(md_routes("/apibinarymarketdata"))
        .with_state(state)
}

async fn serve(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

async fn broker(cfg: &'static XtsConfig) -> (XtsBroker, Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let base = serve(app(fake.clone())).await;
    (XtsBroker::with_base_url(cfg, master(), &base), fake, base)
}

fn creds() -> BrokerCredentials {
    BrokerCredentials {
        api_key: "ikey".into(),
        api_secret: Some("isecret".into()),
        api_key_market: Some("mkey".into()),
        api_secret_market: Some("msecret".into()),
        ..Default::default()
    }
}

fn token() -> AuthToken {
    AuthToken::new(INTERACTIVE)
        .with_feed(Some(MARKET))
        .with_user_id("<USER_ID>")
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn direct_login_stores_both_sessions() {
    let (b, fake, _) = broker(&fivepaisaxts::CONFIG).await;
    let r = b.authenticate(creds()).await.unwrap();
    assert_eq!(r.auth_token, INTERACTIVE);
    assert_eq!(r.feed_token.as_deref(), Some(MARKET));
    assert_eq!(r.user_id, "<USER_ID>");
    let s: Value =
        serde_json::from_str(fake.lines("session|")[0].trim_start_matches("session|")).unwrap();
    assert_eq!(
        s,
        json!({"appKey": "ikey", "secretKey": "isecret", "source": "WebAPI"})
    );
    let m = &fake.lines("mdlogin|")[0];
    assert!(m.starts_with("mdlogin|/apimarketdata/auth/login|"));
    assert!(m.contains("\"appKey\":\"mkey\"") && m.contains("\"source\":\"WebAPI\""));

    // Without market keys the trading session still stands.
    let mut c = creds();
    c.api_key_market = None;
    let r = b.authenticate(c).await.unwrap();
    assert_eq!(r.feed_token, None);
    // Refused market keys: same.
    let mut c = creds();
    c.api_key_market = Some("bad".into());
    assert_eq!(b.authenticate(c).await.unwrap().feed_token, None);
    // Refused interactive keys: an auth error with the broker's reason.
    let mut c = creds();
    c.api_secret = Some("wrong".into());
    let e = b.authenticate(c).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert_eq!(e.client_message(), "Invalid appKey or secretKey");
    let e = b
        .authenticate(BrokerCredentials::default())
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn jainam_sends_access_token_on_binary_market_path() {
    let (b, fake, _) = broker(&jainamxts::CONFIG).await;
    b.authenticate(creds()).await.unwrap();
    let s: Value =
        serde_json::from_str(fake.lines("session|")[0].trim_start_matches("session|")).unwrap();
    assert_eq!(
        s,
        json!({"appKey": "ikey", "secretKey": "isecret", "accessToken": "jainamxts"})
    );
    assert!(fake.lines("mdlogin|")[0].starts_with("mdlogin|/apibinarymarketdata/auth/login|"));
}

#[tokio::test]
async fn oauth_variants() {
    let (b, fake, _) = broker(&compositedge::CONFIG).await;
    let mut c = creds();
    c.request_token = Some(r#"{"accessToken":"acc-1","userID":"<USER_ID>"}"#.into());
    let r = b.authenticate(c).await.unwrap();
    assert_eq!(r.auth_token, INTERACTIVE);
    let s: Value =
        serde_json::from_str(fake.lines("session|")[0].trim_start_matches("session|")).unwrap();
    assert_eq!(s["accessToken"], "acc-1");
    assert!(s.get("source").is_none());
    assert!(
        b.authenticate(creds()).await.is_err(),
        "no callback session"
    );

    let (b, fake, _) = broker(&rmoney::CONFIG).await;
    let mut c = creds();
    c.api_key = String::new();
    c.api_secret = None;
    c.auth_code = Some(r#""{\"token\":\"rm-token\",\"userID\":\"<USER_ID>\"}""#.into());
    let r = b.authenticate(c).await.unwrap();
    assert_eq!(r.auth_token, "rm-token");
    assert_eq!(r.feed_token.as_deref(), Some(MARKET));
    assert!(
        fake.lines("session|").is_empty(),
        "no /user/session for rmoney"
    );
    assert!(fake.lines("mdlogin|")[0].starts_with("mdlogin|/apibinarymarketdata/auth/login|"));
}

// ---------------------------------------------------------------------------
// Orders and books
// ---------------------------------------------------------------------------

fn order(symbol: &str, exchange: &str, qty: i32, pt: &str) -> ResolvedOrder {
    ResolvedOrder::resolve(
        &OrderRequest {
            symbol: symbol.into(),
            exchange: exchange.into(),
            side: "SELL".into(),
            quantity: qty,
            price: 0.0,
            order_type: pt.into(),
            product: "MIS".into(),
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
async fn place_modify_cancel() {
    let (b, fake, _) = broker(&fivepaisaxts::CONFIG).await;
    let t = token();
    let r = b
        .place_order(&t, &order("SBIN", "NSE", 5, "MARKET"))
        .await
        .unwrap();
    assert_eq!(r.order_id, "1200010");
    let line = &fake.lines("place|")[0];
    assert!(line.starts_with(&format!("place|{}|", INTERACTIVE)));
    let body: Value = serde_json::from_str(line.splitn(3, '|').nth(2).unwrap()).unwrap();
    assert_eq!(body["exchangeSegment"], "NSECM");
    assert_eq!(body["exchangeInstrumentID"], 3045);
    assert_eq!(body["orderSide"], "SELL");
    assert_eq!(body["orderType"], "MARKET");
    assert_eq!(body["orderQuantity"], 5);
    assert_eq!(body["orderUniqueIdentifier"], "openalgo");

    let e = b
        .place_order(&t, &order("SBIN", "NSE", 7, "MARKET"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "BROKER_ERROR");
    assert_eq!(
        e.client_message(),
        "Order quantity should be multiple of lot size"
    );

    let m = ResolvedModify::resolve(
        "1200001",
        &ModifyOrderRequest {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "MIS".into(),
            pricetype: "LIMIT".into(),
            quantity: 10,
            price: 2501.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    let r = b.modify_order(&t, &m).await.unwrap();
    assert_eq!(r.order_id, "1200001");
    assert!(fake.lines("modify|")[0].contains("\"modifiedLimitPrice\":2501.0"));

    let r = b.cancel_order(&t, "1200001").await.unwrap();
    assert_eq!(r.order_id, "1200001");
    assert_eq!(fake.lines("cancel|"), ["cancel|appOrderID=1200001"]);
    assert_eq!(
        b.cancel_order(&t, "12&x=1").await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
}

#[tokio::test]
async fn cancel_all_and_close_all() {
    let (b, fake, _) = broker(&fivepaisaxts::CONFIG).await;
    let t = token();
    let r = b.cancel_all_orders(&t).await.unwrap();
    assert_eq!(r.cancelled, ["1200001", "1200003", "1200005"]);
    assert!(r.failed.is_empty());
    let r = b.close_all_positions(&t).await.unwrap();
    assert_eq!(r.placed.len(), 2, "flat position skipped");
    let places = fake.lines("place|");
    assert!(places[0].contains("\"exchangeInstrumentID\":\"2885\""));
    assert!(places[0].contains("\"orderSide\":\"SELL\""));
    assert!(places[1].contains("\"exchangeSegment\":\"NSEFO\""));
    assert!(
        places[1].contains("\"orderSide\":\"BUY\"") && places[1].contains("\"orderQuantity\":50")
    );
    assert_eq!(fake.lines("positions|")[0], "positions|dayOrNet=NetWise");
    let q = b
        .get_open_position(
            &t,
            "NIFTY25APR2422500CE",
            "NFO".parse().unwrap(),
            Product::Nrml,
        )
        .await
        .unwrap();
    assert_eq!(q, -50);
}

#[tokio::test]
async fn books_and_funds() {
    let (b, _, _) = broker(&fivepaisaxts::CONFIG).await;
    let t = token();
    let o = b.get_order_book(&t).await.unwrap();
    assert_eq!(o.len(), 6);
    assert_eq!(o[1].symbol, "NIFTY25APR2422500CE");
    assert_eq!(o[2].status, "trigger pending");
    let tr = b.get_trade_book(&t).await.unwrap();
    assert_eq!(tr[0].symbol, "NIFTY25APR2422500CE");
    let p = b.get_positions(&t).await.unwrap();
    assert_eq!(p[0].symbol, "RELIANCE");
    let h = b.get_holdings(&t).await.unwrap();
    assert!(h.iter().any(|x| x.symbol == "RELIANCE"));
    let f = b.get_funds(&t).await.unwrap();
    assert_eq!(f.available_cash, 0.0, "fivepaisaxts reads BalanceList[0]");
    let e = b
        .get_order_book(&AuthToken::new("expired"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert!(b.get_funds(&AuthToken::new("")).await.is_err());
    assert_eq!(
        b.calculate_margin(&t, &[]).await.unwrap_err().code(),
        "UNSUPPORTED"
    );
}

#[tokio::test]
async fn rmoney_funds_header_and_margin() {
    let (b, fake, _) = broker(&rmoney::CONFIG).await;
    let t = token();
    let f = b.get_funds(&t).await.unwrap();
    assert_eq!(f.available_cash, 85001.0);
    let leg = MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY25APR2422500CE"),
        action: Action::Sell,
        quantity: 50,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    };
    let unknown = MarginLeg {
        key: QuoteKey::new("NFO", "NOPE"),
        ..leg.clone()
    };
    let m = b
        .calculate_margin(&t, &[leg, unknown.clone()])
        .await
        .unwrap();
    assert_eq!(m.total_margin_required, 150.75);
    let sent: Value =
        serde_json::from_str(fake.lines("margin|")[0].trim_start_matches("margin|")).unwrap();
    assert_eq!(sent["portfolio"].as_array().unwrap().len(), 1);
    assert_eq!(sent["portfolio"][0]["exchange"], 2);
    assert_eq!(sent["portfolio"][0]["exchangeInstrumentId"], 43210);
    assert_eq!(sent["portfolio"][0]["orderSessionType"], 1);
    assert_eq!(
        b.calculate_margin(&t, &[unknown]).await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[tokio::test]
async fn quotes_depth_and_multiquotes() {
    let (b, fake, _) = broker(&fivepaisaxts::CONFIG).await;
    let t = token();
    let q = b
        .get_quote(&t, &QuoteKey::new("NFO", "NIFTY25APR2422500CE"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 2500.25);
    assert_eq!(q.oi, 1234500);
    let lines = fake.lines("quotes|");
    assert_eq!(lines[0], format!("quotes|{}|1502|1", MARKET));
    assert_eq!(lines[1], format!("quotes|{}|1510|1", MARKET));
    let d = b
        .get_market_depth(&t, &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.bids[0].quantity, 120);
    let idx = b
        .get_quote(&t, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(idx.ltp, 2500.25);

    // 60 keys -> two quote calls (50 + 10); unknown symbols are reported
    // per key and request order is kept.
    let mut keys = vec![QuoteKey::new("NSE", "NOPE")];
    for _ in 0..59 {
        keys.push(QuoteKey::new("NSE", "SBIN"));
    }
    keys.push(QuoteKey::new("NSE", "RELIANCE"));
    fake.seen.lock().clear();
    let r = b.get_multiquotes(&t, &keys).await.unwrap();
    assert_eq!(r.len(), 61);
    assert!(r[0].error.is_some() && r[0].data.is_none());
    assert_eq!(r[60].symbol, "RELIANCE");
    assert_eq!(r[60].data.as_ref().unwrap().bid_qty, 120);
    assert_eq!(
        fake.lines("quotes|"),
        [
            format!("quotes|{}|1502|50", MARKET),
            format!("quotes|{}|1502|10", MARKET)
        ]
    );
}

#[tokio::test]
async fn rmoney_multiquotes_merge_open_interest() {
    let (b, fake, _) = broker(&rmoney::CONFIG).await;
    let r = b
        .get_multiquotes(&token(), &[QuoteKey::new("NFO", "NIFTY25APR2422500CE")])
        .await
        .unwrap();
    assert_eq!(r[0].data.as_ref().unwrap().oi, 1234500);
    assert_eq!(fake.lines("quotes|").len(), 2);
}

#[tokio::test]
async fn invalid_feed_token_is_renewed_once() {
    let (b, fake, _) = broker(&fivepaisaxts::CONFIG).await;
    b.authenticate(creds()).await.unwrap();
    *fake.expired_feed.lock() = Some("old-feed".into());
    let t = AuthToken::new(INTERACTIVE).with_feed(Some("old-feed"));
    let q = b
        .get_quote(&t, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 2500.25);
    let lines = fake.lines("quotes|");
    assert_eq!(lines[0], "quotes|old-feed|1502|1");
    assert_eq!(lines[1], format!("quotes|{}|1502|1", MARKET));
    assert_eq!(fake.lines("mdlogin|").len(), 2, "login + one renewal");

    // Without market keys in memory the trader is told to log in again.
    let (b, fake, _) = broker(&fivepaisaxts::CONFIG).await;
    *fake.expired_feed.lock() = Some("old-feed".into());
    let e = b
        .get_quote(&t, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
}

#[tokio::test]
async fn history_windows_and_today_candle() {
    let (b, fake, _) = broker(&fivepaisaxts::CONFIG).await;
    let t = token();
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        interval: "1m".into(),
        start: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2024, 1, 10).unwrap(),
    };
    let c = b.get_history(&t, &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].timestamp, 1704273300 - 19800);
    assert_eq!(
        fake.lines("ohlc|"),
        [
            format!(
                "ohlc|{}|NSECM|2885|Jan 01 2024 000000|Jan 06 2024 235959|60",
                MARKET
            ),
            format!(
                "ohlc|{}|NSECM|2885|Jan 07 2024 000000|Jan 10 2024 235959|60",
                MARKET
            ),
        ]
    );
    let idx = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "5m".into(),
        ..req.clone()
    };
    b.get_history(&t, &idx).await.unwrap();
    assert!(fake.lines("ohlc|")[2].contains("|NSECM|26000|"));
    let bad = HistoryRequest {
        interval: "7m".into(),
        ..req.clone()
    };
    assert_eq!(
        b.get_history(&t, &bad).await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
    // Daily with nothing returned up to today: one candle from a quote.
    let today = (chrono::Utc::now() + chrono::Duration::minutes(330)).date_naive();
    let day = HistoryRequest {
        interval: "D".into(),
        start: today,
        end: today,
        ..req
    };
    let c = b.get_history(&t, &day).await.unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].close, 2500.25);
    assert_eq!(c[0].timestamp % 86400, 0);
}

#[tokio::test]
async fn master_download_per_segment_and_indices() {
    let (b, fake, _) = broker(&compositedge::CONFIG).await;
    let rows = b.download_master_contract(&token()).await.unwrap();
    let segs: Vec<String> = fake.lines("master|");
    assert_eq!(
        segs,
        [
            "master|NSECM|",
            "master|NSECD|",
            "master|NSEFO|",
            "master|BSECM|",
            "master|BSEFO|",
            "master|MCXFO|"
        ]
    );
    assert_eq!(fake.lines("indexlist|"), ["indexlist|1", "indexlist|11"]);
    for (ex, sym) in [
        ("NSE", "RELIANCE"),
        ("NFO", "NIFTY25APR2422500CE"),
        ("CDS", "USDINR24MAY2483.25CE"),
        ("MCX", "CRUDEOIL20MAY24FUT"),
        ("NSE_INDEX", "NIFTY"),
        ("BSE_INDEX", "SENSEX"),
    ] {
        assert!(
            rows.iter().any(|r| r.exchange == ex && r.symbol == sym),
            "{} {}",
            ex,
            sym
        );
    }
    assert!(rows
        .iter()
        .all(|r| r.expiry.is_empty() || r.expiry.len() == 9));
}

// ---------------------------------------------------------------------------
// Feed end to end
// ---------------------------------------------------------------------------

/// A fake XTS market-data socket: Engine.IO open, waits for `40`, acks,
/// pings, then streams a 1512 event every 50 ms.
async fn socket_server(log: Arc<Mutex<Vec<String>>>, refuse: bool) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let seen = log.clone();
                let cb = move |req: &Request, resp: Response| {
                    seen.lock().push(format!(
                        "ws|{}|{}",
                        req.uri().path(),
                        req.uri().query().unwrap_or("")
                    ));
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await else {
                    return;
                };
                let _ = ws.send(Message::Text(stream("open"))).await;
                loop {
                    match ws.next().await {
                        Some(Ok(Message::Text(t))) if t == "40" => break,
                        Some(Ok(_)) => continue,
                        _ => return,
                    }
                }
                if refuse {
                    let _ = ws.send(Message::Text(stream("connect_error"))).await;
                    return;
                }
                let _ = ws.send(Message::Text(stream("connect_ack"))).await;
                let _ = ws.send(Message::Text(stream("joined"))).await;
                let _ = ws.send(Message::Text("2".into())).await;
                for _ in 0..200 {
                    tokio::select! {
                        m = ws.next() => match m {
                            Some(Ok(Message::Text(t))) => log.lock().push(format!("eio|{}", t)),
                            Some(Ok(_)) => {}
                            _ => return,
                        },
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {
                            if ws.send(Message::Text(stream("ltp_1512"))).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    url
}

fn sbin() -> FeedSubscription {
    FeedSubscription {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        token: "3045".into(),
        brsymbol: "SBIN".into(),
        brexchange: "NSECM".into(),
        mode: FeedMode::Ltp,
        depth: 5,
    }
}

fn manager() -> WebSocketManager {
    WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(10),
        backoff_max: Duration::from_millis(50),
        ..FeedConfig::default()
    })
}

#[tokio::test]
async fn feed_end_to_end_through_the_relay() {
    let ws_log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket_server(ws_log.clone(), false).await;
    let (b, fake, _) = broker(&fivepaisaxts::CONFIG).await;
    b.authenticate(creds()).await.unwrap();
    let b = b.with_socket_base(&sock);
    let feed = b.create_feed(&token()).unwrap();
    let m = manager();
    let mut ticks = m.subscribe_ticks();
    m.subscribe(vec![sbin()]).await.unwrap();
    m.connect(feed).await.unwrap();
    let tick = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ticks.recv().await {
                Ok(ev) => {
                    if let FeedEvent::Tick(t) = ev.as_ref() {
                        return t.clone();
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("{}", e),
            }
        }
    })
    .await
    .expect("no tick within 10 s");
    assert_eq!((tick.symbol.as_str(), tick.mode), ("SBIN", 1));
    assert_eq!(tick.ltp, 781.35);

    // The socket login (binary path, with source) preceded the connect.
    assert!(fake
        .lines("mdlogin|/apibinarymarketdata/auth/login|")
        .iter()
        .any(|l| l.contains("\"source\":\"WebAPI\"")));
    let ws = ws_log.lock().clone();
    let connect = ws.iter().find(|l| l.starts_with("ws|")).unwrap();
    assert!(connect.starts_with("ws|/apimarketdata/socket.io/|token="));
    assert!(connect.contains("userID=%3CUSER_ID%3E"));
    assert!(connect.contains("publishFormat=JSON&broadcastMode=FULL&EIO=4&transport=websocket"));
    // REST subscription with the socket token.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let subs = fake.lines("subscribe|");
    assert_eq!(subs.len(), 1);
    assert!(subs[0].starts_with(&format!("subscribe|{}|", MARKET)));
    assert!(subs[0].contains("\"xtsMessageCode\":1512"));
    assert!(subs[0].contains("\"exchangeInstrumentID\":3045"));
    // The server ping was answered.
    assert!(ws_log.lock().iter().any(|l| l == "eio|3"));

    m.unsubscribe(vec![sbin()]).await.unwrap();
    // Subscription calls are paced 0.5 s apart.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fake.lines("unsubscribe|").is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(fake.lines("unsubscribe|").len(), 1);
    assert!(fake.lines("unsubscribe|")[0].contains("\"xtsMessageCode\":1512"));
    m.disconnect().await.unwrap();
}

#[tokio::test]
async fn feed_refusal_stops_the_manager() {
    let ws_log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket_server(ws_log, true).await;
    let (b, _, _) = broker(&fivepaisaxts::CONFIG).await;
    let b = b.with_socket_base(&sock);
    let feed = b.create_feed(&token()).unwrap();
    let m = manager();
    let mut status = m.watch_status();
    m.connect(feed).await.unwrap();
    let failed = Arc::new(AtomicBool::new(false));
    let f = failed.clone();
    let _ = tokio::time::timeout(Duration::from_secs(10), async move {
        loop {
            if format!("{:?}", *status.borrow()).contains("AuthFailed") {
                f.store(true, Ordering::SeqCst);
                return;
            }
            if status.changed().await.is_err() {
                return;
            }
        }
    })
    .await;
    assert!(failed.load(Ordering::SeqCst));
    m.disconnect().await.unwrap();
}

#[tokio::test]
async fn feed_without_market_session_reports_why() {
    let (b, _, _) = broker(&fivepaisaxts::CONFIG).await;
    let feed = b.create_feed(&AuthToken::new(INTERACTIVE)).unwrap();
    let m = manager();
    let mut status = m.watch_status();
    m.connect(feed).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(10), async move {
        loop {
            let s = format!("{:?}", *status.borrow());
            if s.contains("AuthFailed") {
                return s;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(msg.contains("market data API key"));
    m.disconnect().await.unwrap();
}
