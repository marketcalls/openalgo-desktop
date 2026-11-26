//! Groww feeds end to end: the shared `WebSocketManager` drives a
//! `GrowwFeed` (prepare, NATS replies) against a fake Groww (socket token
//! endpoint plus a NATS-over-WebSocket server that sends `INFO` with a
//! nonce, checks the `CONNECT` signature, answers `PING` and streams a
//! protobuf tick per `SUB`), and the order feed the broker builds against
//! one that pushes an order update on its F&O order subject (web
//! openalgo#2474).

use axum::routing::post;
use axum::{Json, Router};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{
    FeedEvent, FeedMode, FeedSubscription, OrderFeed, OrderUpdate,
};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::groww::{nkeys, proto, GrowwBroker};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::websocket::handshake;
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

/// Whether a `CONNECT`'s `sig` is its `nkey`'s signature of `nonce`.
fn signed(connect: &Value, nonce: &str) -> bool {
    let pk = nkeys::public_key_bytes(connect["nkey"].as_str().unwrap_or("")).unwrap();
    let sig = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        connect["sig"].as_str().unwrap_or(""),
    )
    .unwrap();
    VerifyingKey::from_bytes(&pk)
        .unwrap()
        .verify(nonce.as_bytes(), &Signature::from_slice(&sig).unwrap())
        .is_ok()
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
                let cb = handshake::check(move |req: &Request, mut resp: Response| {
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
                });
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
                            log.lock().push(format!(
                                "connect jwt={} sig_ok={}",
                                v["jwt"],
                                signed(&v, nonce)
                            ));
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

fn varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn number_field(number: u64, v: u64) -> Vec<u8> {
    let mut out = varint(number << 3);
    out.extend(varint(v));
    out
}

fn text_field(number: u64, v: &[u8]) -> Vec<u8> {
    let mut out = varint((number << 3) | 2);
    out.extend(varint(v.len() as u64));
    out.extend_from_slice(v);
    out
}

/// An `OrderDetailsBroadCastDto` for an executed F&O sell (the web test's
/// `FNO_PUSH`): qty 75 (1), filled 75 (4), the Groww order id (7), status
/// EXECUTED (9 = 6), exchange NSE (11 = 1), segment FNO (12 = 1), product
/// NRML (13 = 4), LIMIT (14 = 1), SELL (15 = 1) and the contract (22).
fn order_push() -> Vec<u8> {
    let mut inner = Vec::new();
    for (n, v) in [
        (1, 75),
        (4, 75),
        (9, 6),
        (11, 1),
        (12, 1),
        (13, 4),
        (14, 1),
        (15, 1),
    ] {
        inner.extend(number_field(n, v));
    }
    inner.extend(text_field(7, b"GMKFO250214150557M2HR6EJF2HSE"));
    inner.extend(text_field(22, b"NIFTY2522025400CE"));
    let mut out = text_field(1, &text_field(2, b"ORDER_PLACED"));
    out.extend(text_field(2, &inner));
    out
}

/// Fake Groww NATS server for the order subjects: `INFO` with a nonce,
/// then, once a frame has carried the F&O order subject's `SUB`, that
/// subject's order push.
async fn order_nats_server(log: Log) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let hlog = log.clone();
                let cb = handshake::check(move |req: &Request, mut resp: Response| {
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
                });
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await else {
                    return;
                };
                let nonce = "nonce-456";
                let _ = ws
                    .send(Message::Text(format!(
                        "INFO {{\"server_id\":\"t\",\"nonce\":\"{}\",\"headers\":true}}\r\n",
                        nonce
                    )))
                    .await;
                while let Some(Ok(m)) = ws.next().await {
                    let Message::Text(text) = m else { continue };
                    log.lock()
                        .push(format!("frame of {} ops", text.matches("\r\n").count()));
                    let mut fno_sid = None;
                    for op in text.split("\r\n").filter(|s| !s.is_empty()) {
                        if let Some(json) = op.strip_prefix("CONNECT ") {
                            let v: Value = serde_json::from_str(json).unwrap();
                            log.lock().push(format!(
                                "connect jwt={} sig_ok={}",
                                v["jwt"],
                                signed(&v, nonce)
                            ));
                        } else if let Some(rest) = op.strip_prefix("SUB ") {
                            log.lock().push(format!("sub {}", rest));
                            if rest.starts_with("stocks_fo/") {
                                fno_sid = rest.split(' ').nth(1).map(str::to_string);
                            }
                        } else if op == "PING" {
                            let _ = ws.send(Message::Text("PONG\r\n".into())).await;
                        }
                    }
                    if let Some(sid) = fno_sid {
                        let payload = order_push();
                        let mut frame = format!(
                            "MSG stocks_fo/order/updates.apex.sub-1 {} {}\r\n",
                            sid,
                            payload.len()
                        )
                        .into_bytes();
                        frame.extend_from_slice(&payload);
                        frame.extend_from_slice(b"\r\n");
                        let _ = ws.send(Message::Binary(frame)).await;
                    }
                }
            });
        }
    });
    url
}

/// Web openalgo#2474: the broker's order feed is the order subjects of
/// Groww's NATS server, not the order-book poller. Its own socket token,
/// one handshake frame (a signed `CONNECT`, both order subjects, `PING`),
/// and the order push decoded into an update on the order socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_updates_flow_from_the_order_subjects() {
    let log: Log = Arc::default();
    let token_url = token_server(log.clone()).await;
    let ws_url = order_nats_server(log.clone()).await;
    let broker = GrowwBroker::with_base_url(SymbolResolver::new(), "http://127.0.0.1:9")
        .with_feed_endpoints(&token_url, &ws_url);
    let Ok(OrderFeed::Socket(feed)) = broker.create_order_feed(&AuthToken::new("session-token"))
    else {
        panic!("Groww's order feed is its order socket");
    };

    let manager = WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(10),
        backoff_max: Duration::from_millis(50),
        ..FeedConfig::default()
    });
    let mut events = manager.subscribe_ticks();
    manager.connect(feed).await.unwrap();

    let update: OrderUpdate = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(ev) => {
                    if let FeedEvent::OrderUpdate(u) = ev.as_ref() {
                        return u.clone();
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("{}", e),
            }
        }
    })
    .await
    .expect("no order update within 10 s");
    assert_eq!(update.orderid, "GMKFO250214150557M2HR6EJF2HSE");
    assert_eq!(
        (update.exchange.as_str(), update.symbol.as_str()),
        ("NFO", "NIFTY2522025400CE")
    );
    assert_eq!(
        (update.action.as_str(), update.order_status.as_str()),
        ("SELL", "complete")
    );
    assert_eq!((update.quantity, update.filled_quantity), (75, 75));

    let log = log.lock().clone();
    assert!(log[0].starts_with("socketKey=U"), "{:?}", log);
    for line in [
        "headers auth=Bearer jwt-x sub=sub-1 proto=nats",
        "frame of 4 ops",
        "connect jwt=\"jwt-x\" sig_ok=true",
        "sub stocks/order/updates.apex.sub-1 1",
        "sub stocks_fo/order/updates.apex.sub-1 2",
    ] {
        assert!(log.contains(&line.to_string()), "{} in {:?}", line, log);
    }
    manager.disconnect().await.unwrap();
}
