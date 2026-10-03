//! The Upstox market-data and order-update feeds end to end through the
//! shared `WebSocketManager`: a fake authorize endpoint hands out a
//! loopback socket address, a fake Upstox socket answers the binary JSON
//! subscribe frame with a protobuf `FeedResponse`, and the manager
//! publishes normalised ticks. A refused login stops the feed.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::upstox::proto;
use openalgo_desktop_lib::brokers::upstox::{master_contract, UpstoxBroker, Urls};
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::websocket::{FeedConfig, FeedStatus, WebSocketManager};
use parking_lot::Mutex;
use prost::Message as _;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.feed.signature";

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

fn sub(symbol: &str, exchange: &str, mode: FeedMode) -> FeedSubscription {
    let row = master().by_symbol(exchange, symbol).unwrap();
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: row.token.clone(),
        brsymbol: row.brsymbol.clone(),
        brexchange: row.brexchange.clone(),
        mode,
        depth: 5,
    }
}

fn ltpc_frame(key: &str, ltp: f64) -> Vec<u8> {
    proto::FeedResponse {
        r#type: proto::Type::LiveFeed as i32,
        feeds: [(
            key.to_string(),
            proto::Feed {
                feed_union: Some(proto::feed::FeedUnion::Ltpc(proto::Ltpc {
                    ltp,
                    ltt: 1758448799000,
                    ltq: 10,
                    cp: 1398.0,
                    iep: None,
                })),
                request_mode: proto::RequestMode::Ltpc as i32,
            },
        )]
        .into_iter()
        .collect(),
        current_ts: 1758448800000,
        market_info: None,
    }
    .encode_to_vec()
}

#[derive(Default)]
struct Fake {
    ws_url: Mutex<String>,
    authorizes: AtomicUsize,
    refuse: std::sync::atomic::AtomicBool,
    subs: Mutex<Vec<Value>>,
    bearer: Mutex<Vec<String>>,
}

/// Fake Upstox socket: answers each `sub` frame with one LTPC tick per key,
/// and on the order stream pushes one order update.
async fn ws_server(fake: Arc<Fake>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            let fake = fake.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                if fake.ws_url.lock().contains("order") {
                    let update = json!({"update_type": "order", "exchange": "NSE",
                        "instrument_token": "NSE_EQ|INE062A01020", "trading_symbol": "SBIN-EQ",
                        "status": "complete", "order_id": "1", "quantity": 1, "filled_quantity": 1,
                        "transaction_type": "BUY", "product": "D", "average_price": 571.0});
                    let _ = ws.send(Message::Text(update.to_string())).await;
                }
                while let Some(Ok(m)) = ws.next().await {
                    if let Message::Binary(b) = m {
                        let v: Value = serde_json::from_slice(&b).unwrap();
                        fake.subs.lock().push(v.clone());
                        if v["method"] == "sub" {
                            for k in v["data"]["instrumentKeys"].as_array().unwrap() {
                                let frame = ltpc_frame(k.as_str().unwrap(), 1410.2);
                                if ws.send(Message::Binary(frame)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    url
}

async fn api(fake: Arc<Fake>) -> String {
    let authorize = |State(f): State<Arc<Fake>>, h: HeaderMap| async move {
        f.authorizes.fetch_add(1, Ordering::SeqCst);
        f.bearer
            .lock()
            .push(h["authorization"].to_str().unwrap().to_string());
        if f.refuse.load(Ordering::SeqCst) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(
                    json!({"status": "error", "errors": [{"errorCode": "UDAPI100050", "message": "Invalid token used to access API"}]}),
                ),
            );
        }
        let url = f.ws_url.lock().clone();
        (
            StatusCode::OK,
            Json(
                json!({"status": "success", "data": {"authorized_redirect_uri": format!("{}?code=single-use&X-Amz-Signature=abc", url)}}),
            ),
        )
    };
    let app = Router::new()
        .route("/v3/feed/market-data-feed/authorize", get(authorize))
        .route("/v2/feed/portfolio-stream-feed/authorize", get(authorize))
        .with_state(fake);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    format!("http://{}", addr)
}

fn manager() -> WebSocketManager {
    WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(50),
        stall_timeout: Duration::from_secs(10),
        connect_timeout: Duration::from_secs(5),
        ..FeedConfig::default()
    })
}

async fn next_event(
    rx: &mut tokio::sync::broadcast::Receiver<openalgo_desktop_lib::websocket::MarketEvent>,
) -> FeedEvent {
    let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("no event within 10 s")
        .unwrap();
    (*ev).clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn market_feed_authorizes_subscribes_and_publishes_ticks() {
    let fake = Arc::new(Fake::default());
    *fake.ws_url.lock() = format!("{}/market-data-feed", ws_server(fake.clone()).await);
    let base = api(fake.clone()).await;
    let broker = UpstoxBroker::with_urls(master(), Urls::local(&base));
    let m = manager();
    let mut rx = m.subscribe_ticks();
    m.subscribe(vec![sub("RELIANCE", "NSE", FeedMode::Ltp)])
        .await
        .unwrap();
    m.connect(broker.create_feed(&AuthToken::new(TOKEN)).unwrap())
        .await
        .unwrap();
    let FeedEvent::Tick(t) = next_event(&mut rx).await else {
        panic!("expected a tick")
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    assert_eq!(t.ltp, 1410.2);
    assert_eq!(t.close, 1398.0);
    assert!(m.is_connected());
    assert_eq!(fake.bearer.lock()[0], format!("Bearer {}", TOKEN));
    let first = fake.subs.lock()[0].clone();
    assert_eq!(first["data"]["mode"], "ltpc");
    assert_eq!(
        first["data"]["instrumentKeys"],
        json!(["NSE_EQ|INE002A01018"])
    );

    // A new subscription on the live socket goes out as one frame.
    m.subscribe(vec![sub("NIFTY06OCT2624500CE", "NFO", FeedMode::Quote)])
        .await
        .unwrap();
    let FeedEvent::Tick(t) = next_event(&mut rx).await else {
        panic!("expected a tick")
    };
    assert_eq!(t.symbol, "NIFTY06OCT2624500CE");
    assert_eq!(fake.subs.lock()[1]["data"]["mode"], "full");
    m.disconnect().await.unwrap();
    assert!(!m.is_running());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_login_stops_the_feed() {
    let fake = Arc::new(Fake::default());
    fake.refuse.store(true, Ordering::SeqCst);
    *fake.ws_url.lock() = format!("{}/market-data-feed", ws_server(fake.clone()).await);
    let base = api(fake.clone()).await;
    let broker = UpstoxBroker::with_urls(master(), Urls::local(&base));
    let m = manager();
    let mut status = m.watch_status();
    m.connect(broker.create_feed(&AuthToken::new(TOKEN)).unwrap())
        .await
        .unwrap();
    let st = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let FeedStatus::AuthFailed { message, .. } = status.borrow_and_update().clone() {
                return message;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(st.contains("Log in to Upstox again"));
    // No hammering after a refusal.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.authorizes.load(Ordering::SeqCst), 1);
    m.disconnect().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_fetches_a_fresh_authorized_url() {
    let fake = Arc::new(Fake::default());
    *fake.ws_url.lock() = format!("{}/market-data-feed", ws_server(fake.clone()).await);
    let base = api(fake.clone()).await;
    let broker = UpstoxBroker::with_urls(master(), Urls::local(&base));
    let m = manager();
    let mut rx = m.subscribe_ticks();
    m.subscribe(vec![sub("SBIN", "NSE", FeedMode::Ltp)])
        .await
        .unwrap();
    m.connect(broker.create_feed(&AuthToken::new(TOKEN)).unwrap())
        .await
        .unwrap();
    next_event(&mut rx).await;
    // Restart the feed: the signed URL is single use, so authorize again.
    m.connect(broker.create_feed(&AuthToken::new(TOKEN)).unwrap())
        .await
        .unwrap();
    next_event(&mut rx).await;
    assert_eq!(fake.authorizes.load(Ordering::SeqCst), 2);
    m.disconnect().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_stream_publishes_order_updates() {
    let fake = Arc::new(Fake::default());
    let url = ws_server(fake.clone()).await;
    *fake.ws_url.lock() = format!("{}/order", url);
    let base = api(fake.clone()).await;
    let broker = UpstoxBroker::with_urls(master(), Urls::local(&base));
    let m = manager();
    let mut rx = m.subscribe_ticks();
    m.connect(broker.order_socket(&AuthToken::new(TOKEN)).unwrap())
        .await
        .unwrap();
    let FeedEvent::OrderUpdate(u) = next_event(&mut rx).await else {
        panic!("expected an order update")
    };
    assert_eq!(u.symbol, "SBIN");
    assert_eq!(u.order_status, "complete");
    assert_eq!(u.product, "CNC");
    m.disconnect().await.unwrap();
}
