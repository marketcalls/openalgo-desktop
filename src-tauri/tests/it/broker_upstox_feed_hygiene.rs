//! Resource hygiene of the Upstox feed relay (CLAUDE.md "Measure, do not
//! just read"): 60 connect / tick / disconnect cycles through the shared
//! manager, each starting a loopback relay, an authorize call and a broker
//! socket, must leave descriptors flat. The test runs in its own
//! process (`isolated!`) so parallel tests do not disturb the count.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::upstox::proto;
use openalgo_desktop_lib::brokers::upstox::{UpstoxBroker, Urls};
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::websocket::{FeedConfig, WebSocketManager};
use prost::Message as _;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[cfg(unix)]
fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
}

#[cfg(not(unix))]
fn open_fds() -> usize {
    0
}

fn frame() -> Vec<u8> {
    proto::FeedResponse {
        r#type: proto::Type::LiveFeed as i32,
        feeds: [(
            "NSE_EQ|INE062A01020".to_string(),
            proto::Feed {
                feed_union: Some(proto::feed::FeedUnion::Ltpc(proto::Ltpc {
                    ltp: 571.0,
                    ..Default::default()
                })),
                request_mode: 0,
            },
        )]
        .into_iter()
        .collect(),
        current_ts: 1,
        market_info: None,
    }
    .encode_to_vec()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_cycles_do_not_leak_descriptors() {
    crate::isolated!(relay_cycles_do_not_leak_descriptors);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = Arc::new(format!("ws://{}/feed?code=1", l.local_addr().unwrap()));
    let ws_server = tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                while let Some(Ok(m)) = ws.next().await {
                    if m.is_binary() && ws.send(Message::Binary(frame())).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let app = Router::new()
        .route(
            "/v3/feed/market-data-feed/authorize",
            get(|State(u): State<Arc<String>>| async move {
                Json(json!({"status": "success", "data": {"authorized_redirect_uri": u.as_str()}}))
            }),
        )
        .with_state(ws_url);
    let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", api.local_addr().unwrap());
    let api_server = tokio::spawn(async move {
        let _ = axum::serve(api, app).await;
    });

    let symbols = SymbolResolver::new();
    let broker = UpstoxBroker::with_urls(symbols, Urls::local(&base));
    let auth = AuthToken::new("eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.hygiene.sig");
    let m = WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(50),
        ..FeedConfig::default()
    });
    let sub = FeedSubscription {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        token: "NSE_EQ|INE062A01020".into(),
        brsymbol: "SBIN".into(),
        brexchange: "NSE_EQ".into(),
        mode: FeedMode::Ltp,
        depth: 5,
    };
    let mut rx = m.subscribe_ticks();

    // Warm up: first connections allocate runtime and pool state.
    for _ in 0..5 {
        run_cycle(&m, &broker, &auth, &sub, &mut rx).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = open_fds();
    for _ in 0..60 {
        run_cycle(&m, &broker, &auth, &sub, &mut rx).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = open_fds();
    eprintln!(
        "upstox relay hygiene: fds before={} after={}",
        before, after
    );
    assert!(
        after <= before + 4,
        "descriptor leak over relay cycles: {} -> {}",
        before,
        after
    );
    assert!(!m.is_running());
    ws_server.abort();
    api_server.abort();
}

async fn run_cycle(
    m: &WebSocketManager,
    broker: &UpstoxBroker,
    auth: &AuthToken,
    sub: &FeedSubscription,
    rx: &mut tokio::sync::broadcast::Receiver<openalgo_desktop_lib::websocket::MarketEvent>,
) {
    m.subscribe(vec![sub.clone()]).await.unwrap();
    m.connect(broker.create_feed(auth).unwrap()).await.unwrap();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("no tick within 10 s");
        match ev {
            Ok(e) if matches!(*e, FeedEvent::Tick(_)) => break,
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(e) => panic!("tick channel closed: {}", e),
        }
    }
    m.disconnect().await.unwrap();
}
