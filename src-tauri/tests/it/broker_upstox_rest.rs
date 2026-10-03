//! Upstox adapter against a local fake Upstox (ephemeral port): auth code
//! exchange, orders on the HFT host, books, funds (including the
//! service-hours refusal), margin, quotes, multiquotes (with an LTP-only
//! indicator), depth, history path order, GTT and the gzip master.

use axum::body::Bytes;
use axum::extract::{Form, OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::upstox::{master_contract, UpstoxBroker, Urls};
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.test.signature";

fn books() -> Value {
    serde_json::from_str(include_str!("../fixtures/brokers/upstox/books.json")).unwrap()
}

fn market() -> Value {
    serde_json::from_str(include_str!("../fixtures/brokers/upstox/market.json")).unwrap()
}

fn orders_fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/brokers/upstox/orders.json")).unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(
        master_contract::parse_json(
            include_str!("../fixtures/brokers/upstox/instruments.json").as_bytes(),
        )
        .unwrap(),
    );
    r
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<String>>,
    funds_locked: std::sync::atomic::AtomicBool,
    quote_hits: AtomicUsize,
}

type S = State<Arc<Fake>>;

fn log(s: &Fake, line: String) {
    s.seen.lock().push(line);
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

fn gz_master() -> Vec<u8> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(include_str!("../fixtures/brokers/upstox/instruments.json").as_bytes())
        .unwrap();
    enc.finish().unwrap()
}

fn app(state: Arc<Fake>) -> Router {
    Router::new()
        .route(
            "/v2/login/authorization/token",
            post(
                |State(s): S, Form(f): Form<HashMap<String, String>>| async move {
                    log(
                        &s,
                        format!(
                            "token|{}|{}|{}|{}|{}",
                            f["code"],
                            f["client_id"],
                            f["client_secret"],
                            f["redirect_uri"],
                            f["grant_type"]
                        ),
                    );
                    if f["code"] == "bad" {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(books()["token_refused"].clone()),
                        );
                    }
                    (StatusCode::OK, Json(books()["token_ok"].clone()))
                },
            ),
        )
        .route(
            "/v3/order/place",
            post(
                |State(s): S, h: HeaderMap, Json(b): Json<Value>| async move {
                    log(
                        &s,
                        format!(
                            "place|{}|{}|{}",
                            h["authorization"].to_str().unwrap(),
                            h["accept"].to_str().unwrap(),
                            b
                        ),
                    );
                    if b["order_type"] == "SL-M" && b["price"].as_f64() != Some(0.0) {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(books()["order_rejected"].clone()),
                        );
                    }
                    if b["quantity"] == 999 {
                        return (
                            StatusCode::TOO_MANY_REQUESTS,
                            Json(books()["rate_limited"].clone()),
                        );
                    }
                    (StatusCode::OK, Json(books()["place_ok"].clone()))
                },
            ),
        )
        .route(
            "/v3/order/modify",
            put(|State(s): S, Json(b): Json<Value>| async move {
                log(&s, format!("modify|{}", b));
                Json(books()["modify_ok"].clone())
            }),
        )
        .route(
            "/v3/order/cancel",
            delete(|State(s): S, OriginalUri(u): OriginalUri| async move {
                let q = u.query().unwrap_or_default().to_string();
                log(&s, format!("cancel|{}", q));
                let id = q.trim_start_matches("order_id=").to_string();
                Json(json!({"status": "success", "data": {"order_id": id}}))
            }),
        )
        .route(
            "/v2/order/retrieve-all",
            get(|| async { Json(orders_fixture()) }),
        )
        .route(
            "/v2/order/trades/get-trades-for-day",
            get(|| async { Json(books()["trades"].clone()) }),
        )
        .route(
            "/v2/portfolio/short-term-positions",
            get(|| async { Json(books()["positions"].clone()) }),
        )
        .route(
            "/v2/portfolio/long-term-holdings",
            get(|| async { Json(books()["holdings"].clone()) }),
        )
        .route(
            "/v3/user/get-funds-and-margin",
            get(|State(s): S, h: HeaderMap| async move {
                log(&s, format!("funds|{}", h["api-version"].to_str().unwrap()));
                if s.funds_locked.load(Ordering::SeqCst) {
                    return (
                        StatusCode::LOCKED,
                        Json(books()["funds_service_hours"].clone()),
                    );
                }
                (StatusCode::OK, Json(books()["funds_v3"].clone()))
            }),
        )
        .route(
            "/v2/charges/margin",
            post(|State(s): S, Json(b): Json<Value>| async move {
                log(&s, format!("margin|{}", b));
                Json(books()["margin"].clone())
            }),
        )
        .route(
            "/v3/market-quote/quotes",
            get(|State(s): S, OriginalUri(u): OriginalUri| async move {
                let n = s.quote_hits.fetch_add(1, Ordering::SeqCst);
                log(&s, format!("quotes|{}", u.query().unwrap_or_default()));
                // The very first call is refused once to exercise the retry.
                if n == 0 {
                    let mut h = HeaderMap::new();
                    h.insert("retry-after", "0".parse().unwrap());
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        h,
                        Json(books()["rate_limited"].clone()),
                    )
                        .into_response();
                }
                Json(market()["quotes"].clone()).into_response()
            }),
        )
        .route(
            "/v2/market-quote/ltp",
            get(|State(s): S, OriginalUri(u): OriginalUri| async move {
                log(&s, format!("ltp|{}", u.query().unwrap_or_default()));
                Json(market()["indicator_ltp"].clone())
            }),
        )
        .route(
            "/v3/historical-candle/{*rest}",
            get(|State(s): S, OriginalUri(u): OriginalUri| async move {
                log(&s, format!("history|{}", u.path()));
                if u.path().contains("/minutes/") {
                    return Json(market()["history_minute"].clone());
                }
                Json(market()["history_day"].clone())
            }),
        )
        .route(
            "/v3/order/gtt/place",
            post(|State(s): S, Json(b): Json<Value>| async move {
                log(&s, format!("gtt_place|{}", b));
                Json(market()["gtt_place_ok"].clone())
            }),
        )
        .route(
            "/v3/order/gtt/cancel",
            delete(|State(s): S, body: Bytes| async move {
                log(&s, format!("gtt_cancel|{}", String::from_utf8_lossy(&body)));
                Json(json!({"status": "success", "data": {"gtt_order_ids": ["GTT-C26210900001"]}}))
            }),
        )
        .route(
            "/v3/order/gtt",
            get(|| async { Json(market()["gtt_book"].clone()) }),
        )
        .route("/master.json.gz", get(|| async { gz_master() }))
        .with_state(state)
}

async fn setup() -> (UpstoxBroker, Arc<Fake>) {
    let state = Arc::new(Fake::default());
    let base = serve(app(state.clone())).await;
    (UpstoxBroker::with_urls(master(), Urls::local(&base)), state)
}

fn auth() -> AuthToken {
    AuthToken::new(TOKEN)
}

fn seen(s: &Fake, prefix: &str) -> Vec<String> {
    s.seen
        .lock()
        .iter()
        .filter(|l| l.starts_with(prefix))
        .cloned()
        .collect()
}

fn order(symbol: &str, exchange: &str, pricetype: &str, qty: i32) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: qty,
        price: 101.5,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: Some(100.0),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[tokio::test]
async fn code_exchange_posts_the_remembered_redirect() {
    let (b, s) = setup().await;
    openalgo_desktop_lib::brokers::catalog::authorize_url(
        "upstox",
        "apikey",
        "http://127.0.0.1:5500/upstox/callback",
        "state1",
    )
    .unwrap();
    let r = b
        .authenticate(BrokerCredentials {
            api_key: "apikey".into(),
            api_secret: Some("secret".into()),
            auth_code: Some("code123".into()),
            request_token: Some("code123".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(r.auth_token.starts_with("eyJ"));
    assert_eq!(r.user_id, "<USER_ID>");
    assert_eq!(
        seen(&s, "token")[0],
        "token|code123|apikey|secret|http://127.0.0.1:5500/upstox/callback|authorization_code"
    );
    let e = b
        .authenticate(BrokerCredentials {
            api_key: "apikey".into(),
            api_secret: Some("secret".into()),
            auth_code: Some("bad".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert!(e.client_message().contains("Invalid Auth code"));
    let e = b
        .authenticate(BrokerCredentials {
            api_key: "apikey".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn orders_go_to_the_hft_v3_endpoints() {
    let (b, s) = setup().await;
    let r = b
        .place_order(&auth(), &order("NIFTY06OCT2624500CE", "NFO", "LIMIT", 75))
        .await
        .unwrap();
    // A sliced order returns one id per slice; the first is reported.
    assert_eq!(r.order_id, "1644490272000");
    let line = &seen(&s, "place")[0];
    assert!(line.starts_with(&format!("place|Bearer {}|application/json|", TOKEN)));
    let body: Value = serde_json::from_str(line.splitn(4, '|').nth(3).unwrap()).unwrap();
    assert_eq!(body["instrument_token"], "NSE_FO|40551");
    assert_eq!(body["product"], "I");
    assert_eq!(body["price"], 101.5);
    assert_eq!(body["trigger_price"], 0.0);
    assert_eq!(body["tag"], "openalgo");
    // A mutation is never retried on a rate-limit refusal.
    let e = b
        .place_order(&auth(), &order("SBIN", "NSE", "MARKET", 999))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("limiting requests"));
    assert_eq!(seen(&s, "place").len(), 2);

    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "CNC".into(),
        pricetype: "LIMIT".into(),
        quantity: 2,
        price: 570.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("1644490272000", &m, &master()).unwrap();
    assert_eq!(
        b.modify_order(&auth(), &rm).await.unwrap().order_id,
        "1644490272000"
    );
    assert!(seen(&s, "modify")[0].contains("\"order_id\":\"1644490272000\""));
    let c = b.cancel_order(&auth(), "260921025562882").await.unwrap();
    assert_eq!(c.order_id, "260921025562882");
    assert_eq!(seen(&s, "cancel")[0], "cancel|order_id=260921025562882");
}

#[tokio::test]
async fn cancel_all_and_close_all() {
    let (b, s) = setup().await;
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    // Raw "trigger pending" and "open" only.
    assert_eq!(r.cancelled, ["260921025562881", "260921025562882"]);
    assert!(r.failed.is_empty());
    assert_eq!(seen(&s, "cancel").len(), 2);
    let r = b.close_all_positions(&auth()).await.unwrap();
    assert_eq!(r.placed.len(), 2, "{:?}", r.failed);
    let placed = seen(&s, "place");
    assert!(placed[0].contains("\"transaction_type\":\"SELL\""));
    assert!(placed[0].contains("\"instrument_token\":\"NSE_EQ|INE062A01020\""));
    assert!(placed[0].contains("\"product\":\"D\""));
    assert!(placed[1].contains("\"transaction_type\":\"BUY\""));
    assert!(placed[1].contains("\"quantity\":75"));
    assert_eq!(
        b.get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Cnc)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        b.get_open_position(&auth(), "NIFTY06OCT2624500CE", Exchange::Nfo, Product::Mis)
            .await
            .unwrap(),
        -75
    );
    assert_eq!(
        b.get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn books_return_openalgo_symbols() {
    let (b, _) = setup().await;
    let o = b.get_order_book(&auth()).await.unwrap();
    assert_eq!(o[0].symbol, "SBIN");
    assert_eq!(o[1].status, "open");
    let t = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(t[1].symbol, "NIFTY06OCT2624500CE");
    let p = b.get_positions(&auth()).await.unwrap();
    assert_eq!(p[0].average_price, 571.0);
    let h = b.get_holdings(&auth()).await.unwrap();
    assert_eq!(h[0].symbol, "NHPC");
}

#[tokio::test]
async fn funds_and_margin() {
    let (b, s) = setup().await;
    let f = b.get_funds(&auth()).await.unwrap();
    assert_eq!(f.available_cash, 125000.5);
    assert_eq!(f.collateral, 40000.0);
    assert_eq!(f.utilised_debits, 35000.25);
    assert_eq!(f.m2m_realized, -40.0);
    assert_eq!(f.m2m_unrealized, 302.5);
    assert_eq!(seen(&s, "funds")[0], "funds|3.0");
    s.funds_locked.store(true, Ordering::SeqCst);
    assert_eq!(b.get_funds(&auth()).await.unwrap(), Funds::default());

    let leg = MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
        action: Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    };
    let m = b
        .calculate_margin(&auth(), std::slice::from_ref(&leg))
        .await
        .unwrap();
    assert_eq!(m.total_margin_required, 165802.5);
    assert!(seen(&s, "margin")[0].contains("\"instrument_key\":\"NSE_FO|52168\""));
    let too_many: Vec<MarginLeg> = (0..21).map(|_| leg.clone()).collect();
    assert!(b
        .calculate_margin(&auth(), &too_many)
        .await
        .unwrap_err()
        .client_message()
        .contains("maximum 20"));
}

#[tokio::test]
async fn quotes_retry_once_then_match_and_indicators_use_ltp() {
    let (b, s) = setup().await;
    let q = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 1410.2);
    assert_eq!(q.close, 1398.0);
    let quotes = seen(&s, "quotes");
    assert_eq!(quotes.len(), 2, "429 retried once");
    assert_eq!(quotes[1], "quotes|instrument_key=NSE_EQ%7CINE002A01018");

    let m = b
        .get_multiquotes(
            &auth(),
            &[
                QuoteKey::new("NSE", "RELIANCE"),
                QuoteKey::new("NOPE", "XYZ"),
                QuoteKey::new("GLOBAL_INDEX", "BRENTOIL"),
                QuoteKey::new("NSE_INDEX", "NIFTY"),
                QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
            ],
        )
        .await
        .unwrap();
    assert_eq!(m.len(), 5);
    assert_eq!(m[0].data.as_ref().unwrap().ltp, 1410.2);
    assert!(m[1].error.is_some());
    assert_eq!(m[2].data.as_ref().unwrap().ltp, 66.42);
    assert_eq!(m[3].data.as_ref().unwrap().ltp, 24512.35);
    assert_eq!(m[4].error.as_deref(), Some("No quote data available"));
    // One batched quotes call with every non-indicator key, one LTP call.
    let batch = seen(&s, "quotes").last().unwrap().clone();
    assert!(batch.contains("NSE_EQ%7CINE002A01018"));
    assert!(batch.contains("%2C"));
    assert_eq!(
        seen(&s, "ltp")[0],
        "ltp|instrument_key=GLOBAL_INDICATOR%7CBZUSD"
    );

    let d = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks[0].price, 1410.3);
}

#[tokio::test]
async fn history_uses_to_then_from_and_normalises_daily() {
    let (b, s) = setup().await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        interval: "D".into(),
        start: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 2);
    // Sorted oldest first, UTC midnight of the IST date.
    assert_eq!(c[0].timestamp, 1789603200);
    assert_eq!(c[1].timestamp, 1789689600);
    assert_eq!(
        seen(&s, "history")[0],
        "history|/v3/historical-candle/NSE_EQ%7CINE002A01018/days/1/2026-09-18/2026-09-15"
    );
    let mut req = req;
    req.interval = "5m".into();
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c[0].timestamp, 1789703100);
    assert!(seen(&s, "history")[1].contains("/minutes/5/2026-09-18/2026-09-15"));
    req.interval = "7m".into();
    assert!(b.get_history(&auth(), &req).await.is_err());
}

#[tokio::test]
async fn gtt_place_cancel_and_book() {
    let (b, s) = setup().await;
    let req = GttRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        trigger_type: GttTriggerType::Single,
        action: Action::Buy,
        product: Product::Cnc,
        quantity: 10,
        pricetype: PriceType::Limit,
        price: 1350.0,
        trigger_price: 1350.0,
        triggerprice_sl: 0.0,
        stoploss: 0.0,
        triggerprice_tg: 0.0,
        target: 0.0,
        last_price: None,
    };
    let r = b.place_gtt(&auth(), &req).await.unwrap();
    assert_eq!(r.trigger_id, "GTT-C26210900009");
    let body: Value =
        serde_json::from_str(seen(&s, "gtt_place")[0].trim_start_matches("gtt_place|")).unwrap();
    assert_eq!(body["type"], "SINGLE");
    assert_eq!(body["instrument_token"], "NSE_EQ|INE002A01018");
    // Last price came from a quote: 1350 < 1410.2 -> BELOW.
    assert_eq!(body["rules"][0]["trigger_type"], "BELOW");
    let c = b.cancel_gtt(&auth(), "GTT-C26210900001").await.unwrap();
    assert_eq!(c.trigger_id, "GTT-C26210900001");
    assert_eq!(
        seen(&s, "gtt_cancel")[0],
        "gtt_cancel|{\"gtt_order_id\":\"GTT-C26210900001\"}"
    );
    let book = b.get_gtt_book(&auth(), false).await.unwrap();
    assert_eq!(book.len(), 2);
}

#[tokio::test]
async fn master_contract_downloads_and_decompresses() {
    let (b, _) = setup().await;
    let rows = b.download_master_contract(&auth()).await.unwrap();
    assert_eq!(rows.len(), 19);
    assert!(rows.iter().any(|r| r.symbol == "NIFTY27OCT26FUT"));
}

#[tokio::test]
async fn expired_session_and_bad_token() {
    let state = Arc::new(Fake::default());
    let app = Router::new()
        .route(
            "/v2/order/retrieve-all",
            get(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(books()["token_expired"].clone()),
                )
            }),
        )
        .with_state(state);
    let base = serve(app).await;
    let b = UpstoxBroker::with_urls(master(), Urls::local(&base));
    let e = b.get_order_book(&auth()).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    // A malformed token is refused before any call.
    let e = b.get_order_book(&AuthToken::new("x")).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
}
