//! Groww feed end to end: the shared `WebSocketManager` drives a
//! `GrowwFeed` (prepare, NATS replies) against a fake Groww (socket
//! token endpoint plus a NATS-over-WebSocket server that sends `INFO` with
//! a nonce, checks the `CONNECT` signature, answers `PING` and streams a
//! protobuf tick per `SUB`). Also checks that the order poller owns exactly
//! one task.

use axum::routing::post;
use axum::{Json, Router};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::groww::{nkeys, proto, GrowwBroker};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::websocket::{FeedConfig, WebSocketManager};
use parking_lot::Mutex;
use prost::Message as _;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

type Log = Arc<Mutex<Vec<String>>>;

async fn token_server(log: Log) -> String {
    let app = Router::new().route(
        "/token",
        post(move |Json(b): Json<Value>| async move {
            log.lock().push(format!(
                "socketKey={}",
                b["socketKey"].as_str().unwrap_or("")
            ));
            Json(json!({"token": "jwt-x", "subscriptionId": "sub-1"}))
        }),
    );
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/token", l.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    url
}

fn tick(ltp: f64) -> Vec<u8> {
    proto::LiveData {
        symbol: "SBIN".into(),
        exchange: 1,
        ltp_data: Some(proto::StocksLivePrice {
            ts_in_millis: 1759722011000.0,
            ltp,
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

/// Fake NATS-over-WebSocket server.
async fn nats_server(log: Log) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let hlog = log.clone();
                let cb = move |req: &Request, mut resp: Response| {
                    let h = |k: &str| {
                        req.headers()
                            .get(k)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string()
                    };
                    hlog.lock().push(format!(
                        "headers auth={} sub={} proto={}",
                        h("authorization"),
                        h("x-subscription-id"),
                        h("sec-websocket-protocol")
                    ));
                    resp.headers_mut()
                        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("nats"));
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await else {
                    return;
                };
                let nonce = "nonce-123";
                let _ = ws
                    .send(Message::Text(format!(
                        "INFO {{\"server_id\":\"t\",\"nonce\":\"{}\",\"headers\":true}}\r\n",
                        nonce
                    )))
                    .await;
                let mut public = String::new();
                while let Some(Ok(m)) = ws.next().await {
                    let text = match &m {
                        Message::Text(t) => t.clone(),
                        Message::Binary(b) => String::from_utf8_lossy(b).to_string(),
                        _ => continue,
                    };
                    for op in text.split("\r\n").filter(|s| !s.is_empty()) {
                        if let Some(json) = op.strip_prefix("CONNECT ") {
                            let v: Value = serde_json::from_str(json).unwrap();
                            public = v["nkey"].as_str().unwrap_or("").to_string();
                            let pk = nkeys::public_key_bytes(&public).unwrap();
                            let sig = base64::Engine::decode(
                                &base64::engine::general_purpose::STANDARD,
                                v["sig"].as_str().unwrap_or(""),
                            )
                            .unwrap();
                            let ok = VerifyingKey::from_bytes(&pk)
                                .unwrap()
                                .verify(nonce.as_bytes(), &Signature::from_slice(&sig).unwrap())
                                .is_ok();
                            log.lock()
                                .push(format!("connect jwt={} sig_ok={}", v["jwt"], ok));
                        } else if op == "PING" {
                            let _ = ws.send(Message::Text("PONG\r\n".into())).await;
                        } else if let Some(rest) = op.strip_prefix("SUB ") {
                            let mut parts = rest.split(' ');
                            let subject = parts.next().unwrap_or("").to_string();
                            let sid = parts.next().unwrap_or("0").to_string();
                            log.lock().push(format!("sub {} {}", subject, sid));
                            let payload = tick(812.35);
                            let mut frame =
                                format!("MSG {} {} {}\r\n", subject, sid, payload.len())
                                    .into_bytes();
                            frame.extend_from_slice(&payload);
                            frame.extend_from_slice(b"\r\n");
                            let _ = ws.send(Message::Binary(frame)).await;
                        }
                    }
                }
                log.lock().push(format!("closed {}", !public.is_empty()));
            });
        }
    });
    url
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticks_flow_through_relay_and_manager() {
    let log: Log = Arc::default();
    let token_url = token_server(log.clone()).await;
    let ws_url = nats_server(log.clone()).await;
    let broker = GrowwBroker::with_base_url(SymbolResolver::new(), "http://127.0.0.1:9")
        .with_feed_endpoints(&token_url, &ws_url);
    let feed = broker
        .create_feed(&AuthToken::new("session-token"))
        .unwrap();

    let manager = WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(10),
        backoff_max: Duration::from_millis(50),
        ..FeedConfig::default()
    });
    let mut ticks = manager.subscribe_ticks();
    manager
        .subscribe(vec![FeedSubscription {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            token: "3045".into(),
            brsymbol: "SBIN".into(),
            brexchange: "NSE".into(),
            mode: FeedMode::Ltp,
            depth: 5,
        }])
        .await
        .unwrap();
    manager.connect(feed).await.unwrap();

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
    assert_eq!(
        (tick.symbol.as_str(), tick.exchange.as_str(), tick.mode),
        ("SBIN", "NSE", 1)
    );
    assert_eq!(tick.ltp, 812.35);
    assert_eq!(tick.last_trade_time_ms, 1759722011000);

    let log = log.lock().clone();
    assert!(log[0].starts_with("socketKey=U"), "{:?}", log);
    assert!(
        log.contains(&"headers auth=Bearer jwt-x sub=sub-1 proto=nats".to_string()),
        "{:?}",
        log
    );
    assert!(
        log.contains(&"connect jwt=\"jwt-x\" sig_ok=true".to_string()),
        "{:?}",
        log
    );
    assert!(
        log.contains(&"sub /ld/eq/nse/price.3045 1".to_string()),
        "{:?}",
        log
    );
    manager.disconnect().await.unwrap();
}

/// 100 start/stop cycles leave no poller task behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_poller_owns_one_task() {
    let broker = GrowwBroker::with_base_url(SymbolResolver::new(), "http://127.0.0.1:9");
    let auth = AuthToken::new("tok");
    let metrics = tokio::runtime::Handle::current().metrics();
    let before = metrics.num_alive_tasks();
    let mut receivers = Vec::new();
    for _ in 0..100 {
        receivers.push(
            broker
                .start_order_updates(&auth, Duration::from_secs(5))
                .unwrap(),
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Each start replaced (aborted) the previous poller.
    assert_eq!(metrics.num_alive_tasks(), before + 1);
    assert!(broker.order_updates_running());
    broker.stop_order_updates();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(metrics.num_alive_tasks(), before);
    // Every receiver sees its poller gone.
    for mut rx in receivers {
        assert!(rx.recv().await.is_none());
    }
}
