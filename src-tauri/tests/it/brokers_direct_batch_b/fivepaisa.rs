//! 5paisa adapter suite against a local fake broker (ephemeral port):
//! TOTP sign-in, the head/body envelope, MPP-protected MARKET and SL-M
//! orders, modify/cancel through the order book's exchange order id,
//! cancel-all, close-all, open position, books, funds, quotes, batched
//! multiquotes, depth, chunked history and the scrip master download.

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::fivepaisa::master_contract::parse_csv;
use openalgo_desktop_lib::brokers::fivepaisa::{FivepaisaBroker, Session};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use openalgo_desktop_lib::error::AppError;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const MASTER: &str = include_str!("../../fixtures/brokers/fivepaisa/ScripMaster.csv");
const RESPONSES: &str = include_str!("../../fixtures/brokers/fivepaisa/responses.json");
const JWT: &str =
    "eyJhbGciOiJIUzI1NiJ9.eyJSZWRpcmVjdFNlcnZlciI6IkIiLCJ1bmlxdWVfbmFtZSI6IjxVU0VSX0lEPiJ9.c2ln";

fn resp(name: &str) -> Value {
    let all: Value = serde_json::from_str(RESPONSES).unwrap();
    all[name].clone()
}

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    query: String,
    authorization: String,
    body: Value,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
}

impl Fake {
    fn calls(&self, leaf: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.path.ends_with(leaf))
            .cloned()
            .collect()
    }

    fn history_calls(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.path.starts_with("/V2/historical/"))
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

fn snapshot_for(body: &Value) -> Value {
    let rows: Vec<Value> = body["body"]["Data"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|d| {
            let code = d["ScripCode"].as_str().unwrap_or("0").to_string();
            let n: i64 = code.parse().unwrap_or(0);
            let ltp = if n == 3045 {
                812.5
            } else {
                100.0 + n as f64 % 7.0
            };
            json!({"ScripCode": n, "Symbol": "", "LastTradedPrice": ltp, "Open": ltp - 1.0,
                   "High": ltp + 2.0, "Low": ltp - 3.0, "PClose": ltp - 4.25, "Volume": 1000,
                   "LastTradedQty": 5, "OpenInterest": 0})
        })
        .collect();
    json!({"head": {"statusDescription": "Success", "status": "0"}, "body": {"Data": rows, "Status": 0}})
}

fn route(path: &str, body: &Value, auth: &str) -> Response {
    if auth == "bearer expired" {
        return (StatusCode::UNAUTHORIZED, "").into_response();
    }
    if path.starts_with("/V2/historical/") {
        return if path.ends_with("/1d") {
            ok(resp("history_daily"))
        } else {
            ok(resp("history"))
        };
    }
    let leaf = path.rsplit('/').next().unwrap_or("");
    let b = &body["body"];
    match leaf {
        "TOTPLogin" => {
            if b["TOTP"] == "000000" {
                ok(resp("totp_login_bad"))
            } else {
                ok(resp("totp_login"))
            }
        }
        "GetAccessToken" => ok(resp("access_token")),
        "OrderBook" => ok(resp("order_book")),
        "TradeBook" => ok(resp("trade_book")),
        "NetPositionNetWise" => ok(resp("positions")),
        "Holding" => ok(resp("holdings")),
        "Margin" => ok(resp("margin")),
        "MarketSnapshot" => ok(snapshot_for(body)),
        "MarketDepth" => ok(resp("depth_sbin")),
        "PlaceOrderRequest" => {
            if b["Qty"] == 999 {
                ok(resp("place_rejected"))
            } else {
                ok(resp("place_ok"))
            }
        }
        "ModifyOrderRequest" => ok(resp("modify_ok")),
        "CancelOrderRequest" => ok(resp("cancel_ok")),
        "master.csv" => (StatusCode::OK, MASTER).into_response(),
        _ => (StatusCode::NOT_FOUND, "no route").into_response(),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: Bytes| {
        let fake = fake.clone();
        async move {
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let authorization = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let path = uri.path().to_string();
            fake.seen.lock().push(Seen {
                path: path.clone(),
                query: uri.query().unwrap_or_default().to_string(),
                authorization: authorization.clone(),
                body: body.clone(),
            });
            route(&path, &body, &authorization)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

fn symbols() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_csv(MASTER));
    r
}

async fn broker() -> (FivepaisaBroker, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let base = serve(fake.clone()).await;
    let b = FivepaisaBroker::with_urls(symbols(), base.clone(), format!("{}/master.csv", base))
        .with_batch_pause(Duration::from_millis(1));
    (b, fake)
}

fn auth() -> AuthToken {
    AuthToken::new(
        Session {
            api_key: "APPKEY".into(),
            client_code: "50001234".into(),
            access_token: JWT.into(),
        }
        .encode(),
    )
}

fn order(symbol: &str, exchange: &str, pricetype: &str, qty: i32, trigger: f64) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: qty,
        price: 812.0,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: Some(trigger),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &symbols()).unwrap()
}

#[tokio::test]
async fn fivepaisa_sign_in_runs_both_steps() {
    let (b, fake) = broker().await;
    let creds = BrokerCredentials {
        api_key: "APPKEY:::APPUSER:::50001234".into(),
        api_secret: Some("ENCKEY".into()),
        client_id: Some("<EMAIL>".into()),
        password: Some("1234".into()),
        totp: Some("123456".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    assert_eq!(r.auth_token, format!("APPKEY:::50001234:::{}", JWT));
    assert_eq!(r.user_id, "50001234");
    let login = &fake.calls("TOTPLogin")[0].body;
    assert_eq!(
        login,
        &json!({"head":{"Key":"APPKEY"},"body":{"Email_ID":"<EMAIL>","TOTP":"123456","PIN":"1234"}})
    );
    let token = &fake.calls("GetAccessToken")[0].body;
    assert_eq!(
        token["body"],
        json!({"RequestToken":"req-tok-1","EncryKey":"ENCKEY","UserId":"APPUSER"})
    );

    let mut bad = creds.clone();
    bad.totp = Some("000000".into());
    let e = b.authenticate(bad).await.unwrap_err();
    assert!(matches!(e, AppError::Auth(_)));
    assert!(e.client_message().contains("Invalid TOTP"));

    let mut malformed = creds;
    malformed.api_key = "APPKEY".into();
    let before = fake.seen.lock().len();
    let e = b.authenticate(malformed).await.unwrap_err();
    assert!(matches!(e, AppError::Validation(_)));
    assert_eq!(
        fake.seen.lock().len(),
        before,
        "no call with a malformed key"
    );
}

#[tokio::test]
async fn fivepaisa_orders_use_the_envelope_and_price_protection() {
    let (b, fake) = broker().await;
    let a = auth();
    let r = b
        .place_order(&a, &order("SBIN", "NSE", "LIMIT", 10, 0.0))
        .await
        .unwrap();
    assert_eq!(r.order_id, "402189542");
    let call = fake.calls("PlaceOrderRequest").remove(0);
    assert_eq!(call.authorization, format!("bearer {}", JWT));
    assert_eq!(call.body["head"], json!({"key": "APPKEY"}));
    assert_eq!(call.body["body"]["Price"], json!(812.0));
    assert_eq!(call.body["body"]["ScripCode"], json!("3045"));
    assert_eq!(call.body["body"]["IsIntraday"], json!(true));

    // MARKET: LTP 812.5 + 0.5 % slab, rounded to the 0.05 tick.
    b.place_order(&a, &order("SBIN", "NSE", "MARKET", 10, 0.0))
        .await
        .unwrap();
    let snap = fake.calls("MarketSnapshot");
    assert_eq!(snap[0].body["body"]["ClientCode"], "50001234");
    assert_eq!(
        snap[0].body["body"]["Data"][0],
        json!({"Exchange":"N","ExchangeType":"C","ScripCode":"3045","ScripData":""})
    );
    assert_eq!(
        fake.calls("PlaceOrderRequest")[1].body["body"]["Price"],
        json!(816.55)
    );

    // SL-M: a stop-limit one slab beyond the trigger.
    b.place_order(&a, &order("SBIN", "NSE", "SL-M", 10, 810.0))
        .await
        .unwrap();
    let slm = &fake.calls("PlaceOrderRequest")[2].body["body"];
    assert_eq!(
        (slm["Price"].clone(), slm["StopLossPrice"].clone()),
        (json!(814.05), json!(810.0))
    );

    let e = b
        .place_order(&a, &order("SBIN", "NSE", "LIMIT", 999, 0.0))
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), "Insufficient funds in your account");

    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "LIMIT".into(),
        quantity: 12,
        price: 811.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("402189541", &m, &symbols()).unwrap();
    assert_eq!(b.modify_order(&a, &rm).await.unwrap().order_id, "402189541");
    assert_eq!(
        fake.calls("ModifyOrderRequest")[0].body["body"],
        json!({"ExchOrderID":"1100000012345678","Price":811.0,"Qty":12,"StopLossPrice":0.0,"DisQty":0})
    );
    let missing = ResolvedModify::resolve("1", &m, &symbols()).unwrap();
    assert!(b.modify_order(&a, &missing).await.is_err());

    b.cancel_order(&a, "402189541").await.unwrap();
    assert_eq!(
        fake.calls("CancelOrderRequest")[0].body["body"],
        json!({"ExchOrderID":"1100000012345678"})
    );
    assert!(b.cancel_order(&a, "nope").await.is_err());
}

#[tokio::test]
async fn fivepaisa_cancel_all_close_all_open_position() {
    let (b, fake) = broker().await;
    let a = auth();
    let r = b.cancel_all_orders(&a).await.unwrap();
    assert_eq!(r.cancelled, ["402189541", "402189543"]);
    assert!(r.failed.is_empty());
    assert_eq!(fake.calls("CancelOrderRequest").len(), 2);

    let c = b.close_all_positions(&a).await.unwrap();
    assert_eq!(c.placed.len(), 2, "{:?}", c.failed);
    let placed = fake.calls("PlaceOrderRequest");
    let fut = &placed[0].body["body"];
    assert_eq!(
        (
            fut["OrderType"].clone(),
            fut["ExchangeType"].clone(),
            fut["Qty"].clone(),
            fut["IsIntraday"].clone()
        ),
        (json!("B"), json!("D"), json!(75), json!(false))
    );
    let rel = &placed[1].body["body"];
    assert_eq!(
        (rel["OrderType"].clone(), rel["IsIntraday"].clone()),
        (json!("S"), json!(true))
    );

    let fut_qty = b
        .get_open_position(&a, "NIFTY27OCT26FUT", Exchange::Nfo, Product::Nrml)
        .await
        .unwrap();
    assert_eq!(fut_qty, -75);
    let rel_qty = b
        .get_open_position(&a, "RELIANCE", Exchange::Nse, Product::Mis)
        .await
        .unwrap();
    assert_eq!(rel_qty, 3);
    let none = b
        .get_open_position(&a, "RELIANCE", Exchange::Nse, Product::Cnc)
        .await
        .unwrap();
    assert_eq!(none, 0);
}

#[tokio::test]
async fn fivepaisa_books_and_funds() {
    let (b, fake) = broker().await;
    let a = auth();
    let orders = b.get_order_book(&a).await.unwrap();
    assert_eq!(orders[1].symbol, "NIFTY27OCT26FUT");
    assert!(orders.iter().all(|o| o.status == o.status.to_lowercase()));
    assert_eq!(
        fake.calls("OrderBook")[0].body,
        json!({"head":{"key":"APPKEY"},"body":{"ClientCode":"50001234"}})
    );
    let trades = b.get_trade_book(&a).await.unwrap();
    assert_eq!(trades[1].symbol, "RELIANCE");
    let pos = b.get_positions(&a).await.unwrap();
    assert_eq!(pos[0].quantity, -75);
    let h = b.get_holdings(&a).await.unwrap();
    assert_eq!(h[0].symbol, "SBIN");
    let f = b.get_funds(&a).await.unwrap();
    assert_eq!((f.available_cash, f.m2m_realized), (100487.24, 1230.5));
    assert!(b
        .calculate_margin(&a, &[])
        .await
        .is_err_and(|e| matches!(e, AppError::Unsupported(_))));

    let expired = AuthToken::new(
        Session {
            api_key: "APPKEY".into(),
            client_code: "50001234".into(),
            access_token: "expired".into(),
        }
        .encode(),
    );
    let e = b.get_order_book(&expired).await.unwrap_err();
    assert!(matches!(e, AppError::Auth(_)));
}

#[tokio::test]
async fn fivepaisa_quotes_multiquotes_depth() {
    let (b, fake) = broker().await;
    let a = auth();
    let q = b
        .get_quote(&a, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.close),
        (812.5, 812.45, 812.55, 808.25)
    );

    // Index names on NSE are looked up on NSE_INDEX (exchange type C).
    let idx = b
        .get_quote(&a, &QuoteKey::new("NSE", "NIFTY"))
        .await
        .unwrap();
    assert!(idx.ltp > 0.0);
    let last_snap = fake.calls("MarketSnapshot").pop().unwrap();
    assert_eq!(last_snap.body["body"]["Data"][0]["ScripCode"], "999920000");

    let d = b
        .get_market_depth(&a, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(
        (d.bids[0].price, d.asks[0].price, d.ltq),
        (812.45, 812.55, 5)
    );
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (470, 130));

    let keys = vec![
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
    ];
    let res = b.get_multiquotes(&a, &keys).await.unwrap();
    assert_eq!(res.len(), 3);
    assert_eq!(res[0].data.as_ref().unwrap().ltp, 812.5);
    assert!(res[1].data.is_none() && res[1].error.is_some());
    assert_eq!(res[2].symbol, "NIFTY27OCT26FUT");
    assert!(res[2].data.is_some());
}

#[tokio::test]
async fn fivepaisa_multiquotes_go_in_batches_of_fifty() {
    let fake = Arc::new(Fake::default());
    let base = serve(fake.clone()).await;
    let r = SymbolResolver::new();
    let rows: Vec<SymToken> = (0..120)
        .map(|i| SymToken {
            symbol: format!("S{}", i),
            brsymbol: format!("S{}", i),
            name: format!("S{}", i),
            exchange: "NSE".into(),
            brexchange: "NSE".into(),
            token: format!("{}", 10_000 + i),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "EQ".into(),
            tick_size: 0.05,
        })
        .collect();
    r.load(rows);
    let b =
        FivepaisaBroker::with_urls(r, base, "unused").with_batch_pause(Duration::from_millis(1));
    let keys: Vec<QuoteKey> = (0..120)
        .map(|i| QuoteKey::new("NSE", format!("S{}", i)))
        .collect();
    let res = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(res.len(), 120);
    assert!(res.iter().all(|r| r.data.is_some()));
    assert_eq!(res[119].symbol, "S119");
    let sizes: Vec<usize> = fake
        .calls("MarketSnapshot")
        .iter()
        .map(|c| c.body["body"]["Data"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, [50, 50, 20]);
}

#[tokio::test]
async fn fivepaisa_history_is_chunked() {
    let (b, fake) = broker().await;
    let a = auth();
    let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
    let c = b
        .get_history(
            &a,
            &HistoryRequest {
                key: QuoteKey::new("NSE", "SBIN"),
                interval: "1m".into(),
                start: d(2026, 8, 1),
                end: d(2026, 10, 15),
            },
        )
        .await
        .unwrap();
    let calls = fake.history_calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].path, "/V2/historical/N/C/3045/1m");
    assert_eq!(calls[0].query, "from=2026-08-01&end=2026-08-30");
    assert_eq!(calls[2].query, "from=2026-09-30&end=2026-10-15");
    // The same candles in every chunk collapse to one series.
    assert_eq!(c.len(), 3);
    assert!(c.windows(2).all(|w| w[0].timestamp < w[1].timestamp));

    let daily = b
        .get_history(
            &a,
            &HistoryRequest {
                key: QuoteKey::new("NSE_INDEX", "NIFTY"),
                interval: "D".into(),
                start: d(2026, 1, 1),
                end: d(2026, 10, 15),
            },
        )
        .await
        .unwrap();
    let calls = fake.history_calls();
    assert_eq!(calls.len(), 6, "three 100-day chunks");
    assert_eq!(calls[3].path, "/V2/historical/N/C/999920000/1d");
    assert_eq!(daily.len(), 3, "indices keep zero-volume days");

    // 5paisa answers a range with no sessions in it with its latest candles;
    // none of them lies inside the range, so nothing is returned (web #2195).
    let future = b
        .get_history(
            &a,
            &HistoryRequest {
                key: QuoteKey::new("NSE", "SBIN"),
                interval: "1m".into(),
                start: d(2026, 11, 2),
                end: d(2026, 11, 6),
            },
        )
        .await
        .unwrap();
    assert_eq!(fake.history_calls().len(), 7);
    assert!(future.is_empty(), "{future:?}");

    let e = b
        .get_history(
            &a,
            &HistoryRequest {
                key: QuoteKey::new("NSE", "SBIN"),
                interval: "3m".into(),
                start: d(2026, 1, 1),
                end: d(2026, 1, 2),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Validation(_)));
}

#[tokio::test]
async fn fivepaisa_master_download() {
    let (b, _fake) = broker().await;
    let rows = b.download_master_contract(&auth()).await.unwrap();
    assert!(rows.iter().any(|r| r.symbol == "NIFTY27OCT2625000CE"));
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTY" && r.exchange == "NSE_INDEX"));
    assert!(rows
        .iter()
        .filter(|r| r.instrument_type == "FUT")
        .all(|r| r.expiry.len() == 9));
}

#[tokio::test]
async fn fivepaisa_order_updates_poll_the_order_book() {
    let (b, fake) = broker().await;
    // The first poll seeds the snapshot; an unchanged book publishes nothing.
    let mut rx = b
        .start_order_updates(&auth(), Duration::from_millis(1))
        .unwrap();
    assert!(b.order_updates_running());
    let quiet = tokio::time::timeout(Duration::from_millis(1300), rx.recv()).await;
    assert!(
        quiet.is_err(),
        "an unchanged order book publishes no update"
    );
    assert!(fake.calls("OrderBook").len() >= 2);
    b.stop_order_updates();
    assert!(!b.order_updates_running());

    // An expired session ends the poller on its own.
    let expired = AuthToken::new(
        Session {
            api_key: "APPKEY".into(),
            client_code: "50001234".into(),
            access_token: "expired".into(),
        }
        .encode(),
    );
    let mut rx = b
        .start_order_updates(&expired, Duration::from_secs(1))
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap();
    assert!(closed.is_none());
    assert!(!b.order_updates_running());
}
