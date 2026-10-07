//! HDFC Securities InvestRight against a local fake REST host and a fake
//! market-data socket: token exchange, the three-field order address,
//! cancel-all, close-all, books marked to `/fetch-ltp`, funds, quotes from
//! LTP batches plus a feed snapshot, and the public security master.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::hdfcsecurities::master_contract::parse_security_master;
use openalgo_desktop_lib::brokers::hdfcsecurities::proto::{
    packet_type, GenericDto, GenericDtoList, MarketDepthDto, MarketDepthDtoList, MbpData,
};
use openalgo_desktop_lib::brokers::hdfcsecurities::HdfcSecuritiesBroker;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use prost::Message as _;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response as WsResponse};
use tokio_tungstenite::tungstenite::Message;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../fixtures/brokers/hdfcsecurities/", $name))
    };
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    authorization: Option<String>,
    user_agent: String,
    body: Value,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    /// Answer every authenticated call with 401.
    expired: Mutex<bool>,
    /// Feed handshakes: (uri, authorization header).
    ws_handshakes: Mutex<Vec<(String, String)>>,
    ws_frames: Mutex<Vec<Value>>,
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

const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiI8VVNFUl9JRD4ifQ.c2ln";

fn route(fake: &Fake, method: &Method, path: &str, query: &str, body: &Value) -> Response {
    if path == "/oapi/v1/security-master" {
        return (StatusCode::OK, fixture!("security_master.csv")).into_response();
    }
    if path == "/oapi/v1/access-token" {
        return if query.contains("request_token=good") && body["apiSecret"] == "SECRET" {
            ok(json!({ "accessToken": JWT }))
        } else {
            (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                json!({"message": "Invalid request token"}).to_string(),
            )
                .into_response()
        };
    }
    if *fake.expired.lock() {
        return (
            StatusCode::UNAUTHORIZED,
            [("content-type", "application/json")],
            json!({"error": "invalid credentials"}).to_string(),
        )
            .into_response();
    }
    match (method.as_str(), path) {
        ("POST", "/oapi/v1/orders/regular") => {
            if body["security_id"] == "REFUSE" || body["quantity"] == 999 {
                ok(json!({"status": "error", "message": "RMS: Margin Exceeds"}))
            } else {
                ok(json!({"status": "success", "data": {"order_id": "9001"}}))
            }
        }
        ("PUT", p) | ("DELETE", p) if p.starts_with("/oapi/v1/orders/regular/") => {
            let id = p.rsplit('/').next().unwrap_or_default();
            if id == "1006" {
                ok(json!({"status": "error", "message": "Order cannot be cancelled"}))
            } else {
                ok(json!({"status": "success", "data": {"order_id": id}}))
            }
        }
        ("GET", "/oapi/v1/orders") => ok(fixture!("orders.json")),
        ("GET", "/oapi/v1/trades") => ok(fixture!("trades.json")),
        ("GET", "/oapi/v1/portfolio/cumulative-positions") => ok(fixture!("positions.json")),
        ("GET", "/oapi/v1/portfolio/holdings") => ok(fixture!("holdings.json")),
        ("GET", "/oapi/v1/user/margins") => ok(fixture!("margins.json")),
        ("PUT", "/oapi/v1/fetch-ltp") => {
            let rows: Vec<Value> = body["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    let tok: f64 = i["token"].as_str().unwrap().parse().unwrap();
                    let ex = i["exchange"].as_str().unwrap();
                    // BFO answers with a blank exchange, as InvestRight does.
                    let ex = if ex == "BFO" { "" } else { ex };
                    json!({"exchange": ex, "token": i["token"], "ltp": tok / 10.0, "prev_close": tok / 20.0})
                })
                .collect();
            ok(json!({"status": "success", "data": rows}))
        }
        _ => (StatusCode::NOT_FOUND, "no such endpoint").into_response(),
    }
}

async fn serve_rest(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let h = |k: &str| {
                    headers
                        .get(k)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                };
                let path = uri.path().to_string();
                let query = uri.query().unwrap_or_default().to_string();
                fake.seen.lock().push(Seen {
                    method: method.to_string(),
                    path: path.clone(),
                    query: query.clone(),
                    authorization: h("authorization"),
                    user_agent: h("user-agent").unwrap_or_default(),
                    body: body.clone(),
                });
                route(&fake, &method, &path, &query, &body)
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

fn packet_for(scrip: &str) -> Option<GenericDto> {
    let (ty, token) = if let Some(t) = scrip.strip_prefix("NSE_INDEX_") {
        (packet_type::NSE_INDEX, t)
    } else if let Some(t) = scrip.strip_prefix("NFO_") {
        (packet_type::NSE_FO_ALL, t)
    } else if let Some(t) = scrip.strip_prefix("NCD_") {
        (packet_type::NSE_CD_ALL, t)
    } else if let Some(t) = scrip.strip_prefix("NSE_") {
        (packet_type::NSE_CM_ALL, t)
    } else {
        return None;
    };
    let level = |price: f64, buy: bool| MarketDepthDto {
        quantity: 40,
        price,
        number_of_orders: 2,
        buy_flag: buy,
    };
    Some(GenericDto {
        instrument_id: token.parse().ok()?,
        packet_type: ty,
        packet_timestamp: 1_790_000_000_000,
        mbp_data: Some(MbpData {
            last_traded_price: 55.5,
            open_price: 50.0,
            high_price: 60.0,
            low_price: 49.0,
            closing_price: 52.0,
            volume_traded_today: 4321,
            last_trade_quantity: 7,
            total_buy_quantity: 800,
            total_sell_quantity: 900,
            oi: 777,
            market_depth_dto_list: Some(MarketDepthDtoList {
                market_depth_dto: vec![level(55.4, true), level(55.6, false)],
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// Fake feed: records the handshake, answers each subscribe frame with
/// one full packet per scrip.
async fn serve_ws(fake: Arc<Fake>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let fake = fake.clone();
            tokio::spawn(async move {
                let rec = fake.clone();
                let cb = move |req: &Request, resp: WsResponse| {
                    let auth = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    rec.ws_handshakes.lock().push((req.uri().to_string(), auth));
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, cb).await else {
                    return;
                };
                while let Some(Ok(msg)) = ws.next().await {
                    let Message::Text(t) = msg else {
                        if msg.is_close() {
                            break;
                        }
                        continue;
                    };
                    let v: Value = serde_json::from_str(&t).unwrap();
                    fake.ws_frames.lock().push(v.clone());
                    let list = GenericDtoList {
                        generic_dto_list: v["subscribe"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|s| packet_for(s["scripId"].as_str()?))
                            .collect(),
                    };
                    if ws
                        .send(Message::Binary(list.encode_to_vec()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
    format!("ws://{}/wsapi/v1/session", addr)
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_security_master(fixture!("security_master.csv")).unwrap());
    r
}

async fn setup() -> (HdfcSecuritiesBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let host = serve_rest(fake.clone()).await;
    let ws = serve_ws(fake.clone()).await;
    let b = HdfcSecuritiesBroker::with_base_url(
        master(),
        &host,
        format!("{}/oapi/v1/security-master", host),
        ws,
    );
    (b, fake, AuthToken::new("APIKEY1:acc-tok"))
}

fn order(symbol: &str, exchange: &str, product: &str, qty: i32) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "SELL".into(),
        quantity: qty,
        price: 0.0,
        order_type: "MARKET".into(),
        product: product.into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[tokio::test]
async fn hdfcsecurities_login_exchanges_the_request_token() {
    let (b, fake, _) = setup().await;
    let creds = |rt: &str| BrokerCredentials {
        api_key: "APIKEY1".into(),
        api_secret: Some("SECRET".into()),
        request_token: Some(rt.into()),
        ..Default::default()
    };
    let r = b.authenticate(creds("good")).await.unwrap();
    assert_eq!(r.auth_token, format!("APIKEY1:{}", JWT));
    assert_eq!(r.user_id, "<USER_ID>");
    let call = &fake.calls("POST", "/oapi/v1/access-token")[0];
    assert!(call.query.contains("api_key=APIKEY1"));
    assert!(call.query.contains("request_token=good"));
    assert_eq!(call.body, json!({"apiSecret": "SECRET"}));
    assert!(call.authorization.is_none());
    assert!(call.user_agent.starts_with("Mozilla/5.0"));

    let e = b.authenticate(creds("stale")).await.unwrap_err();
    assert!(e.client_message().contains("Invalid request token"));
    let mut no_secret = creds("good");
    no_secret.api_secret = None;
    assert!(b.authenticate(no_secret).await.is_err());
}

#[tokio::test]
async fn hdfcsecurities_orders_carry_the_three_field_address() {
    let (b, fake, auth) = setup().await;
    let r = b
        .place_order(&auth, &order("NIFTY25AUG2624500CE", "NFO", "NRML", 75))
        .await
        .unwrap();
    assert_eq!(r.order_id, "9001");
    let call = &fake.calls("POST", "/oapi/v1/orders/regular")[0];
    assert_eq!(call.authorization.as_deref(), Some("acc-tok"));
    assert!(call.user_agent.starts_with("Mozilla/5.0"));
    assert_eq!(call.query, "api_key=APIKEY1");
    let body = &call.body;
    assert_eq!(body["exchange"], "NSE");
    assert_eq!(body["security_id"], "45001");
    assert_eq!(body["instrument_segment"], "OPTIDX");
    assert_eq!(body["transaction_type"], "SELL");
    assert_eq!(body["order_type"], "MARKET");
    assert_eq!(body["product"], "OVERNIGHT");
    assert_eq!(body["expiry_date"], "20260825");
    assert_eq!(body["underlying_symbol"], "NIFTYEQEQNR");
    assert_eq!(body["option_type"], "CE");
    assert_eq!(body["strike_price"], 24500.0);
    assert_eq!(
        body["external_reference_number"].as_str().unwrap().len(),
        13
    );

    // An error payload under HTTP 200 is a refusal, not a success.
    let e = b
        .place_order(&auth, &order("SBIN", "NSE", "MIS", 999))
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), "RMS: Margin Exceeds");

    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "CNC".into(),
        pricetype: "LIMIT".into(),
        quantity: 5,
        price: 800.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let r = b
        .modify_order(
            &auth,
            &ResolvedModify::resolve("1004", &m, &master()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.order_id, "1004");
    let put = &fake.calls("PUT", "/oapi/v1/orders/regular/1004")[0];
    assert_eq!(put.body["product"], "DELIVERY");
    assert_eq!(put.body["order_type"], "LIMIT");
    assert!(put.body.get("security_id").is_none());

    let r = b.cancel_order(&auth, "1004").await.unwrap();
    assert_eq!(r.order_id, "1004");
    assert_eq!(
        fake.calls("DELETE", "/oapi/v1/orders/regular/1004").len(),
        1
    );
}

#[tokio::test]
async fn hdfcsecurities_cancel_all_close_all_and_open_position() {
    let (b, fake, auth) = setup().await;
    let r = b.cancel_all_orders(&auth).await.unwrap();
    // 1002 trigger pending, 1004 open; 1006 partially traded (refused);
    // 1005 carries cancellation_allowed NO.
    assert_eq!(r.cancelled, ["1002", "1004"]);
    assert_eq!(r.failed, ["1006"]);
    assert!(fake
        .calls("DELETE", "/oapi/v1/orders/regular/1005")
        .is_empty());

    let r = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(r.placed.len(), 2);
    assert!(r.failed.is_empty());
    let placed = fake.calls("POST", "/oapi/v1/orders/regular");
    assert_eq!(placed[0].body["security_id"], "STABANEQNR");
    assert_eq!(placed[0].body["transaction_type"], "SELL");
    assert_eq!(placed[0].body["quantity"], 10);
    assert_eq!(placed[0].body["product"], "INTRADAY");
    assert_eq!(placed[1].body["security_id"], "45001");
    assert_eq!(placed[1].body["transaction_type"], "BUY");
    assert_eq!(placed[1].body["quantity"], 75);
    assert_eq!(placed[1].body["product"], "OVERNIGHT");

    let q = |s: &'static str, e, p| {
        let b = &b;
        let auth = &auth;
        async move { b.get_open_position(auth, s, e, p).await.unwrap() }
    };
    assert_eq!(q("SBIN", Exchange::Nse, Product::Mis).await, 10);
    assert_eq!(q("SBIN", Exchange::Nse, Product::Cnc).await, 0);
    assert_eq!(
        q("NIFTY25AUG2624500CE", Exchange::Nfo, Product::Nrml).await,
        -75
    );
}

#[tokio::test]
async fn hdfcsecurities_books_funds_and_expiry() {
    let (b, fake, auth) = setup().await;
    let orders = b.get_order_book(&auth).await.unwrap();
    assert_eq!(orders.len(), 7);
    assert_eq!(orders[1].symbol, "NIFTY25AUG2624500CE");
    assert_eq!(orders[1].status, "trigger pending");
    assert!(orders
        .iter()
        .all(|o| o.status == o.status.to_ascii_lowercase()));
    let trades = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(trades[0].symbol, "NIFTY25AUG2624500CE");

    let pos = b.get_positions(&auth).await.unwrap();
    // fake LTP = token / 10: SBIN 304.5, option 4500.1
    assert_eq!(pos[0].ltp, 304.5);
    assert_eq!(pos[0].pnl, -8000.0 + 3045.0);
    assert_eq!(pos[1].ltp, 4500.1);
    let ltp_body = &fake.calls("PUT", "/oapi/v1/fetch-ltp")[0].body;
    assert_eq!(
        ltp_body,
        &json!({"data": [{"exchange": "NSE", "token": "3045"},
                         {"exchange": "NFO", "token": "45001"},
                         {"exchange": "NSE", "token": "694"}]})
    );

    let h = b.get_holdings(&auth).await.unwrap();
    assert_eq!(h[0].symbol, "SBIN");
    assert_eq!(h[0].ltp, 304.5);
    assert_eq!(h[1].ltp, 12.0);

    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!(f.available_cash, 100000.5);
    assert_eq!(f.collateral, 2500.0);
    assert_eq!(f.utilised_debits, 184.0);
    assert_eq!(f.m2m_realized, 200.0);

    *fake.expired.lock() = true;
    let e = b.get_order_book(&auth).await.unwrap_err();
    assert!(e.client_message().contains("session has expired"));
    let e = b.get_funds(&auth).await.unwrap_err();
    assert!(e.client_message().contains("session has expired"));
}

#[tokio::test]
async fn hdfcsecurities_quote_and_depth_use_ltp_and_a_feed_snapshot() {
    let (b, fake, auth) = setup().await;
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    // REST LTP wins; OHLC, volume and best bid/ask from the feed.
    assert_eq!(q.ltp, 304.5);
    assert_eq!(q.close, 152.25);
    assert_eq!((q.open, q.high, q.low, q.volume), (50.0, 60.0, 49.0, 4321));
    assert_eq!((q.bid, q.ask, q.bid_qty, q.ask_qty), (55.4, 55.6, 40, 40));
    let (uri, authz) = fake.ws_handshakes.lock()[0].clone();
    assert!(uri.ends_with("/wsapi/v1/session?token=acc-tok&api_key=APIKEY1"));
    assert_eq!(authz, "acc-tok");
    assert_eq!(
        fake.ws_frames.lock()[0],
        json!({"heart_beat": false, "subscribe": [{"scripId": "NSE_3045", "type": "ALL"}]})
    );

    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NFO", "NIFTY25AUG2624500CE"))
        .await
        .unwrap();
    assert_eq!(d.ltp, 4500.1);
    assert_eq!(d.oi, 777);
    assert_eq!((d.bids[0].price, d.asks[0].price), (55.4, 55.6));
    assert_eq!(d.bids.len(), 5);
    assert_eq!((d.total_buy_qty, d.total_sell_qty, d.ltq), (800, 900, 7));

    let e = b
        .get_quote(&auth, &QuoteKey::new("NSE", "NOPE"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("NOPE was not found"));
}

#[tokio::test]
async fn hdfcsecurities_multiquotes_batch_ten_and_fill_oi() {
    let (b, fake, auth) = setup().await;
    let keys: Vec<QuoteKey> = [
        ("NSE", "SBIN"),
        ("NSE", "CIPLA"),
        ("NSE", "M&M"),
        ("NSE", "SWIGGY"),
        ("NSE_INDEX", "NIFTY"),
        ("NSE_INDEX", "NIFTYMIDCAP50"),
        ("NSE_INDEX", "MINIFTY"),
        ("BSE_INDEX", "SENSEX"),
        ("BSE", "M&M"),
        ("BSE", "B999XYZ"),
        ("NFO", "NIFTY25AUG2624500CE"),
        ("BFO", "SENSEX27AUG2680000CE"),
        ("NSE", "UNKNOWN"),
    ]
    .iter()
    .map(|(e, s)| QuoteKey::new(*e, *s))
    .collect();
    let started = std::time::Instant::now();
    let r = b.get_multiquotes(&auth, &keys).await.unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(150));
    assert_eq!(r.len(), 13);
    let batches = fake.calls("PUT", "/oapi/v1/fetch-ltp");
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].body["data"].as_array().unwrap().len(), 10);
    assert_eq!(batches[1].body["data"].as_array().unwrap().len(), 2);
    let sbin = r[0].data.as_ref().unwrap();
    assert_eq!(
        (sbin.ltp, sbin.close, sbin.oi, sbin.open),
        (304.5, 152.25, 0, 0.0)
    );
    let ce = r[10].data.as_ref().unwrap();
    assert_eq!(ce.oi, 777);
    // BFO answered with a blank exchange: recovered from the request.
    assert_eq!(r[11].data.as_ref().unwrap().ltp, 82662.5);
    assert_eq!(
        r[12].error.as_deref(),
        Some("Could not find instrument for NSE:UNKNOWN")
    );
    // Only the derivative legs were sent to the feed.
    let frames = fake.ws_frames.lock().clone();
    assert_eq!(
        frames[0],
        json!({"heart_beat": false, "subscribe": [
            {"scripId": "BFO_826625", "type": "ALL"},
            {"scripId": "NFO_45001", "type": "ALL"}]})
    );
}

#[tokio::test]
async fn hdfcsecurities_history_is_unsupported_and_master_is_public() {
    let (b, fake, auth) = setup().await;
    let e = b
        .get_history(
            &auth,
            &HistoryRequest {
                key: QuoteKey::new("NSE", "SBIN"),
                interval: "D".into(),
                start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        e.client_message(),
        "Historical data is not available for your broker."
    );
    assert!(b.timeframe_map().is_empty());

    let rows = b
        .download_master_contract(&AuthToken::new("expired"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 16);
    let call = &fake.calls("GET", "/oapi/v1/security-master")[0];
    assert!(call.authorization.is_none());
    assert!(call.query.is_empty());
}
