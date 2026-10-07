//! Arrow end to end against a local fake Arrow (ephemeral port): the
//! checksum login, custom `appID` / `token` headers, order bodies, books,
//! funds, margin (single, basket and the per-order fallback), quotes with
//! the INDEX candidate probe and the 100-instrument batch cap, depth,
//! history chunking with OI and paise scaling, and the master download.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::arrow::master_contract::parse_instruments;
use openalgo_desktop_lib::brokers::arrow::{checksum, ArrowBroker, ArrowUrls};
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;

const CSV: &str = include_str!("../fixtures/brokers/arrow/instruments.csv");
const RESPONSES: &str = include_str!("../fixtures/brokers/arrow/responses.json");

fn fixture(key: &str) -> Value {
    let all: Value = serde_json::from_str(RESPONSES).unwrap();
    all[key].clone()
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    app_id: String,
    token: String,
    body: Value,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
}

impl Fake {
    fn calls(&self, method: &str, prefix: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.method == method && s.path.starts_with(prefix))
            .cloned()
            .collect()
    }
}

fn json_resp(status: StatusCode, v: Value) -> Response {
    (
        status,
        [("content-type", "application/json")],
        v.to_string(),
    )
        .into_response()
}

fn ok(v: Value) -> Response {
    json_resp(StatusCode::OK, v)
}

/// Arrow's INDEX vocabulary on the fake: NIFTY by OpenAlgo name, NIFTY IT
/// by uppercased display name; MCX iCOMDEX not served.
fn index_quote(symbol: &str) -> Response {
    match symbol {
        "NIFTY" | "NIFTY IT" => ok(fixture("index_quote")),
        _ => json_resp(
            StatusCode::BAD_REQUEST,
            json!({"status":"error","message":"invalid symbol"}),
        ),
    }
}

fn route(method: &Method, path: &str, body: &Value) -> Response {
    let m = method.as_str();
    match (m, path) {
        ("POST", "/auth/app/authenticate-token") => {
            let good = checksum("APP1", "SECRET1", "rt-good");
            if body["checkSum"] == good.as_str() && body["appID"] == "APP1" {
                ok(fixture("auth_ok"))
            } else {
                json_resp(StatusCode::BAD_REQUEST, fixture("auth_failed"))
            }
        }
        ("POST", "/order/regular") => {
            if body["symbol"] == "INFY-EQ" {
                json_resp(StatusCode::BAD_REQUEST, fixture("place_failed"))
            } else {
                ok(fixture("place_ok"))
            }
        }
        ("PATCH", p) if p.starts_with("/order/regular/") => ok(fixture("modify_ok")),
        ("DELETE", "/order/regular/26100300004") => json_resp(
            StatusCode::BAD_REQUEST,
            json!({"status":"error","message":"Order is already in a final state"}),
        ),
        ("DELETE", p) if p.starts_with("/order/regular/") => {
            (StatusCode::OK, "order cancellation request accepted").into_response()
        }
        ("GET", "/user/orders") => ok(fixture("orders")),
        ("GET", "/user/trades") => ok(fixture("trades")),
        ("GET", "/user/positions") => ok(fixture("positions")),
        ("GET", "/user/holdings") => ok(fixture("holdings")),
        ("GET", "/user/limits") => ok(fixture("limits")),
        ("POST", "/margin/order") => ok(fixture("margin_order")),
        ("POST", "/margin/basket") => {
            let refuse = body["orders"]
                .as_array()
                .map(|a| a.iter().any(|o| o["symbol"] == "SBIN-EQ"))
                .unwrap_or(false);
            if refuse {
                ok(json!({"status":"error","message":"basket not supported"}))
            } else {
                ok(fixture("margin_basket"))
            }
        }
        ("POST", "/info/quote/full") | ("POST", "/info/quote/ltp") => {
            if body["exchange"] == "INDEX" {
                index_quote(body["symbol"].as_str().unwrap_or(""))
            } else {
                ok(fixture("quote_full"))
            }
        }
        ("POST", "/info/quotes/full") => {
            let n = body.as_array().map(|a| a.len()).unwrap_or(0);
            if n > 100 {
                // Arrow's real behaviour over the cap.
                (StatusCode::INTERNAL_SERVER_ERROR, "unable to get quotes").into_response()
            } else {
                ok(fixture("quotes_full"))
            }
        }
        ("GET", p) if p.starts_with("/candle/") => {
            if p.ends_with("/week") {
                json_resp(StatusCode::BAD_REQUEST, fixture("history_error"))
            } else if p.ends_with("/day") {
                ok(fixture("history_day"))
            } else {
                ok(fixture("history_minute"))
            }
        }
        ("GET", "/all") => (StatusCode::OK, CSV).into_response(),
        ("GET", "/info/index-list") => ok(fixture("index_list")),
        _ => (StatusCode::NOT_FOUND, "no such endpoint").into_response(),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let body_v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let h = |k: &str| {
                    headers
                        .get(k)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string()
                };
                fake.seen.lock().push(Seen {
                    method: method.to_string(),
                    path: uri.path().to_string(),
                    query: uri.query().unwrap_or_default().to_string(),
                    app_id: h("appid"),
                    token: h("token"),
                    body: body_v.clone(),
                });
                route(&method, uri.path(), &body_v)
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
    r.load(parse_instruments(CSV).unwrap());
    r
}

async fn setup() -> (ArrowBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let b = ArrowBroker::with_base_url(master(), ArrowUrls::rebased(&host, "ws://127.0.0.1:1"));
    (b, fake, AuthToken::new("APP1:jwt-abc"))
}

fn order(symbol: &str, exchange: &str, pricetype: &str, price: f64) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 10,
        price,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: Some(799.5),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[tokio::test]
async fn login_exchanges_the_request_token_with_a_checksum() {
    let (b, fake, _) = setup().await;
    let creds = BrokerCredentials {
        api_key: "APP1".into(),
        api_secret: Some("SECRET1".into()),
        request_token: Some("rt-good".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    assert_eq!(r.auth_token, "APP1:eyJhbGciOiJIUzI1NiJ9.e30.sig");
    assert_eq!(r.user_id, "<USER_ID>");
    assert_eq!(r.user_name.as_deref(), Some("<USER_NAME>"));
    let call = &fake.calls("POST", "/auth/app/authenticate-token")[0];
    assert_eq!(call.body["token"], "rt-good");
    assert_eq!(
        call.body["checkSum"],
        checksum("APP1", "SECRET1", "rt-good").as_str()
    );

    let bad = BrokerCredentials {
        request_token: Some("rt-stale".into()),
        ..creds.clone()
    };
    let e = b.authenticate(bad).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert!(e.client_message().contains("invalid checksum"));

    let missing = BrokerCredentials {
        api_secret: None,
        ..creds
    };
    assert_eq!(
        b.authenticate(missing).await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
}

#[tokio::test]
async fn orders_carry_custom_headers_and_web_bodies() {
    let (b, fake, auth) = setup().await;
    let r = b
        .place_order(&auth, &order("SBIN", "NSE", "MARKET", 0.0))
        .await
        .unwrap();
    assert_eq!(r.order_id, "26100300099");
    let call = &fake.calls("POST", "/order/regular")[0];
    assert_eq!(call.app_id, "APP1");
    assert_eq!(call.token, "jwt-abc");
    assert_eq!(call.body["symbol"], "SBIN-EQ");
    assert_eq!(call.body["order"], "MKT");
    assert_eq!(call.body["mpp"], true);
    assert_eq!(call.body["product"], "I");
    assert_eq!(call.body["transactionType"], "B");

    b.place_order(&auth, &order("NIFTY27OCT26FUT", "NFO", "SL-M", 0.0))
        .await
        .unwrap();
    let sl = &fake.calls("POST", "/order/regular")[1];
    assert_eq!(sl.body["order"], "SL-MKT");
    assert_eq!(sl.body["triggerPrice"], "799.5");
    assert_eq!(sl.body["symbol"], "NIFTY27OCT26F");

    let e = b
        .place_order(&auth, &order("INFY", "NSE", "LIMIT", 1500.0))
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), "RMS: Margin Exceeds");

    let m = ModifyOrderRequest {
        symbol: "INFY".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "CNC".into(),
        pricetype: "LIMIT".into(),
        quantity: 5,
        price: 1490.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("26100300003", &m, &master()).unwrap();
    let r = b.modify_order(&auth, &rm).await.unwrap();
    assert_eq!(r.order_id, "26100300003");
    let mc = &fake.calls("PATCH", "/order/regular/26100300003")[0];
    assert_eq!(mc.body["price"], "1490");
    assert_eq!(mc.body["product"], "C");
    assert!(mc.body.get("transactionType").is_none());

    // Cancel succeeds on a plain-text 200.
    let c = b.cancel_order(&auth, "26100300003").await.unwrap();
    assert_eq!(c.order_id, "26100300003");
    let e = b.cancel_order(&auth, "26100300004").await.unwrap_err();
    assert!(e.client_message().contains("final state"));
}

#[tokio::test]
async fn cancel_all_close_all_and_open_position() {
    let (b, fake, auth) = setup().await;
    let r = b.cancel_all_orders(&auth).await.unwrap();
    // OPEN, PENDING, TRIGGER_PENDING; the id field stands in for orderNo.
    assert_eq!(r.cancelled, ["26100300002", "26100300003"]);
    assert_eq!(r.failed, ["26100300004"]);
    assert_eq!(fake.calls("DELETE", "/order/regular/").len(), 3);

    let closed = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(closed.placed.len(), 2);
    assert!(closed.failed.is_empty());
    let exits = fake.calls("POST", "/order/regular");
    assert_eq!(exits.len(), 2);
    assert_eq!(exits[0].body["symbol"], "SBIN-EQ");
    assert_eq!(exits[0].body["transactionType"], "S");
    assert_eq!(exits[0].body["product"], "I");
    assert_eq!(exits[0].body["quantity"], "10");
    assert_eq!(exits[1].body["symbol"], "NIFTY27OCT26F");
    assert_eq!(exits[1].body["transactionType"], "B");
    assert_eq!(exits[1].body["quantity"], "65");
    assert_eq!(exits[1].body["product"], "M");
    assert_eq!(exits[1].body["mpp"], true);

    let q = b
        .get_open_position(&auth, "NIFTY27OCT26FUT", Exchange::Nfo, Product::Nrml)
        .await
        .unwrap();
    assert_eq!(q, -65);
    let none = b
        .get_open_position(&auth, "NIFTY27OCT26FUT", Exchange::Nfo, Product::Mis)
        .await
        .unwrap();
    assert_eq!(none, 0);
}

#[tokio::test]
async fn books_are_openalgo_symbols_and_lowercase_statuses() {
    let (b, _fake, auth) = setup().await;
    let orders = b.get_order_book(&auth).await.unwrap();
    assert_eq!(orders.len(), 7);
    let statuses: Vec<&str> = orders.iter().map(|o| o.status.as_str()).collect();
    assert_eq!(
        statuses,
        [
            "complete",
            "trigger pending",
            "open",
            "open",
            "cancelled",
            "rejected",
            "open"
        ]
    );
    assert_eq!(orders[1].symbol, "NIFTY27OCT26FUT");
    let trades = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(trades[1].symbol, "NIFTY06OCT2622400PE");
    let pos = b.get_positions(&auth).await.unwrap();
    assert_eq!(pos[0].symbol, "SBIN");
    assert_eq!(pos[0].product, "MIS");
    let h = b.get_holdings(&auth).await.unwrap();
    assert_eq!(h[0].symbol, "SBIN");
    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!(f.available_cash, 200000.25);
    assert_eq!(f.collateral, 15000.5);
}

#[tokio::test]
async fn expired_session_is_reported_before_any_call() {
    let (b, fake, _) = setup().await;
    let e = b
        .get_order_book(&AuthToken::new("no-app-id"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert!(fake.seen.lock().is_empty());
}

#[tokio::test]
async fn margin_single_basket_and_fallback() {
    let (b, fake, auth) = setup().await;
    let leg = |sym: &str, ex: &str, action| MarginLeg {
        key: QuoteKey::new(ex, sym),
        action,
        quantity: 65,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    };
    let one = b
        .calculate_margin(&auth, &[leg("NIFTY06OCT2622400PE", "NFO", Action::Sell)])
        .await
        .unwrap();
    assert_eq!(one.total_margin_required, 152340.55);

    let two = b
        .calculate_margin(
            &auth,
            &[
                leg("NIFTY06OCT2622400PE", "NFO", Action::Sell),
                leg("NIFTY27OCT26FUT", "NFO", Action::Buy),
            ],
        )
        .await
        .unwrap();
    assert_eq!(two.total_margin_required, 206512.75);
    let basket = &fake.calls("POST", "/margin/basket")[0];
    assert_eq!(basket.body["includePositions"], true);
    assert_eq!(basket.body["orders"].as_array().unwrap().len(), 2);

    // Basket refused: per-order sum.
    let fallback = b
        .calculate_margin(
            &auth,
            &[
                leg("SBIN", "NSE", Action::Buy),
                leg("NIFTY27OCT26FUT", "NFO", Action::Buy),
            ],
        )
        .await
        .unwrap();
    assert_eq!(fallback.total_margin_required, 304681.1);

    let e = b
        .calculate_margin(&auth, &[leg("NOPE", "NSE", Action::Buy)])
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn quotes_depth_and_index_candidates() {
    let (b, fake, auth) = setup().await;
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 815.1);
    assert_eq!(q.bid, 815.05);
    let call = &fake.calls("POST", "/info/quote/full")[0];
    assert_eq!(call.body, json!({"exchange": "NSE", "symbol": "SBIN-EQ"}));

    b.get_quote(&auth, &QuoteKey::new("MCX", "CRUDEOIL19OCT26FUT"))
        .await
        .unwrap();
    assert_eq!(
        fake.calls("POST", "/info/quote/full")[1].body["exchange"],
        "MCXFO"
    );

    // NIFTY IT: OpenAlgo name refused, uppercased display name accepted
    // and cached.
    let it = b
        .get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTYIT"))
        .await
        .unwrap();
    assert_eq!(it.ltp, 25012.35);
    let before = fake.calls("POST", "/info/quote/full").len();
    b.get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTYIT"))
        .await
        .unwrap();
    let after = fake.calls("POST", "/info/quote/full");
    assert_eq!(after.len(), before + 1);
    assert_eq!(after.last().unwrap().body["symbol"], "NIFTY IT");

    // MCX iCOMDEX: every candidate refused, then remembered.
    let e = b
        .get_quote(&auth, &QuoteKey::new("MCX_INDEX", "MCXCRUDEX"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("streaming"));
    let n = fake.seen.lock().len();
    assert!(b
        .get_quote(&auth, &QuoteKey::new("MCX_INDEX", "MCXCRUDEX"))
        .await
        .is_err());
    assert_eq!(fake.seen.lock().len(), n);

    // CDS is never sent.
    let e = b
        .get_quote(&auth, &QuoteKey::new("CDS", "USDINR27OCT26FUT"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("CDS"));
    assert_eq!(fake.seen.lock().len(), n);

    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.bids[0].price, 815.05);
    assert_eq!(d.total_buy_qty, 50000);
}

#[tokio::test]
async fn multiquotes_batch_at_one_hundred_and_report_every_leg() {
    let (b, fake, auth) = setup().await;
    let mut keys: Vec<QuoteKey> = (0..150).map(|_| QuoteKey::new("NSE", "SBIN")).collect();
    keys.push(QuoteKey::new("NFO", "NIFTY27OCT26FUT"));
    keys.push(QuoteKey::new("NSE_INDEX", "NIFTY"));
    keys.push(QuoteKey::new("CDS", "USDINR27OCT26FUT"));
    keys.push(QuoteKey::new("NSE", "NOPE"));
    keys.push(QuoteKey::new("NSE", "INFY"));
    let out = b.get_multiquotes(&auth, &keys).await.unwrap();
    assert_eq!(out.len(), keys.len());
    let sizes: Vec<usize> = fake
        .calls("POST", "/info/quotes/full")
        .iter()
        .map(|c| c.body.as_array().unwrap().len())
        .collect();
    // 100, then 50 SBIN + future + index + INFY (CDS and the unknown skipped).
    assert_eq!(sizes, [100, 53]);
    let find = |sym: &str| out.iter().find(|r| r.symbol == sym).unwrap();
    assert_eq!(find("SBIN").data.as_ref().unwrap().ltp, 815.1);
    assert_eq!(find("NIFTY27OCT26FUT").data.as_ref().unwrap().oi, 1_500_000);
    assert_eq!(find("NIFTY").data.as_ref().unwrap().ltp, 25012.35);
    assert_eq!(
        find("USDINR27OCT26FUT").error.as_deref(),
        Some("Exchange not supported by Arrow quotes")
    );
    assert!(find("NOPE")
        .error
        .as_deref()
        .unwrap()
        .contains("Could not find"));
    assert_eq!(
        find("INFY").error.as_deref(),
        Some("No quote data available")
    );
    // The index went out under its verified INDEX name.
    let last = fake.calls("POST", "/info/quotes/full").pop().unwrap();
    assert!(last
        .body
        .as_array()
        .unwrap()
        .contains(&json!({"exchange": "INDEX", "symbol": "NIFTY"})));
}

#[tokio::test]
async fn history_is_chunked_descaled_and_carries_oi_on_nfo() {
    let (b, fake, auth) = setup().await;
    let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
    let req = HistoryRequest {
        key: QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
        interval: "5m".into(),
        start: d(2026, 1, 1),
        end: d(2026, 3, 31),
    };
    let c = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert!(c[0].timestamp < c[1].timestamp);
    assert_eq!(c[0].open, 810.0);
    assert_eq!(c[1].oi, 12000);
    let calls = fake.calls("GET", "/candle/");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].path, "/candle/nfo/35001/5min");
    assert_eq!(
        calls[0].query,
        "from=2026-01-01T00:00:00&to=2026-03-01T23:59:59&oi=1"
    );
    assert_eq!(
        calls[1].query,
        "from=2026-03-02T00:00:00&to=2026-03-31T23:59:59&oi=1"
    );

    let day = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "D".into(),
        start: d(2020, 1, 1),
        end: d(2026, 10, 1),
    };
    let c = b.get_history(&auth, &day).await.unwrap();
    assert_eq!(c[0].timestamp, 1_790_726_400);
    assert_eq!(c[0].oi, 0);
    let calls = fake.calls("GET", "/candle/nse/26000/day");
    // 2000-day daily chunks.
    assert_eq!(calls.len(), 2);
    assert!(!calls[0].query.contains("oi="));

    let bad = HistoryRequest {
        interval: "W".into(),
        ..day
    };
    let e = b.get_history(&auth, &bad).await.unwrap_err();
    assert_eq!(e.client_message(), "invalid interval");
}

#[tokio::test]
async fn master_download_merges_the_index_list() {
    let (b, fake, auth) = setup().await;
    let rows = b.download_master_contract(&auth).await.unwrap();
    // 19 CSV rows + 2 new index-list rows (Nifty 50 is already present).
    assert_eq!(rows.len(), 21);
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTYMIDCAP50" && r.brexchange == "INDEX"));
    let all = &fake.calls("GET", "/all")[0];
    assert_eq!(
        (all.app_id.as_str(), all.token.as_str()),
        ("APP1", "jwt-abc")
    );
}
