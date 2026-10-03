//! Manager tests against a local fake WebSocket server on an ephemeral port.

use super::*;
use crate::brokers::mock::MockFeed;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

fn fast() -> FeedConfig {
    FeedConfig {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        stall_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(2),
        stable_after: Duration::from_secs(60),
        max_instruments: 3,
        command_capacity: 16,
        event_capacity: 64,
    }
}

fn sub(symbol: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        token: "1".into(),
        brsymbol: symbol.into(),
        brexchange: "NSE".into(),
        mode,
        depth: 5,
    }
}

/// Server that records every text frame per connection index and runs
/// `script(conn_index)`: send these frames, then close (true) or hold.
async fn server(
    script: impl Fn(usize) -> (Vec<String>, bool) + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<(usize, String)>>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<(usize, String)>>> = Arc::default();
    let seen2 = seen.clone();
    let script = Arc::new(script);
    let task = tokio::spawn(async move {
        let mut n = 0usize;
        let mut conns = tokio::task::JoinSet::new();
        while let Ok((tcp, _)) = listener.accept().await {
            let idx = n;
            n += 1;
            let seen = seen2.clone();
            let script = script.clone();
            conns.spawn(async move {
                let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                let (mut w, mut r) = ws.split();
                let (frames, close) = script(idx);
                // Wait for the first client frame (the subscribe) if any is due.
                let reader = tokio::spawn(async move {
                    while let Some(Ok(m)) = r.next().await {
                        if let Message::Text(t) = m {
                            seen.lock().push((idx, t));
                        }
                    }
                });
                tokio::time::sleep(Duration::from_millis(50)).await;
                for f in frames {
                    let _ = w.send(Message::Text(f)).await;
                }
                if close {
                    let _ = w.close().await;
                    reader.abort();
                } else {
                    let _ = reader.await;
                }
            });
        }
    });
    (url, seen, task)
}

async fn wait_for(mut f: impl FnMut() -> bool) {
    for _ in 0..400 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn resubscribes_after_reconnect_and_publishes_ticks() {
    let tick = r#"{"t":"SBIN","x":"NSE","p":954.1}"#.to_string();
    let (url, seen, srv) = server(move |i| (vec![tick.clone()], i == 0)).await;
    let m = WebSocketManager::with_config(fast());
    let mut rx = m.subscribe_ticks();
    m.subscribe(vec![sub("SBIN", FeedMode::Quote)])
        .await
        .unwrap();
    m.connect(Box::new(MockFeed::new(url))).await.unwrap();
    let mut ticks = 0;
    while ticks < 2 {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let FeedEvent::Tick(t) = &*ev {
            assert_eq!(
                (t.symbol.as_str(), t.exchange.as_str(), t.ltp),
                ("SBIN", "NSE", 954.1)
            );
            ticks += 1;
        }
    }
    wait_for(|| seen.lock().iter().any(|(i, _)| *i == 1)).await;
    let frames = seen.lock().clone();
    for conn in [0, 1] {
        assert!(
            frames
                .iter()
                .any(|(i, f)| *i == conn && f.contains("NSE:SBIN:2")),
            "connection {} was not resubscribed: {:?}",
            conn,
            frames
        );
    }
    assert!(m.stats().connects.load(Ordering::Relaxed) >= 2);
    assert!(m.is_connected());
    m.disconnect().await.unwrap();
    assert!(!m.is_running());
    assert_eq!(m.status(), FeedStatus::Disconnected);
    assert_eq!(m.instrument_count(), 0);
    srv.abort();
}

#[tokio::test]
async fn subscriptions_are_reference_counted_with_effective_mode() {
    let (url, seen, srv) = server(|_| (vec![], false)).await;
    let m = WebSocketManager::with_config(fast());
    m.connect(Box::new(MockFeed::new(url))).await.unwrap();
    wait_for(|| m.is_connected()).await;
    m.subscribe(vec![sub("SBIN", FeedMode::Ltp)]).await.unwrap();
    m.subscribe(vec![sub("SBIN", FeedMode::Ltp)]).await.unwrap();
    m.subscribe(vec![sub("SBIN", FeedMode::Quote)])
        .await
        .unwrap();
    assert_eq!(m.instrument_count(), 1);
    assert_eq!(m.subscriptions()[0].mode, FeedMode::Quote);
    m.unsubscribe(vec![sub("SBIN", FeedMode::Quote)])
        .await
        .unwrap();
    assert_eq!(m.subscriptions()[0].mode, FeedMode::Ltp);
    m.unsubscribe(vec![sub("SBIN", FeedMode::Ltp)])
        .await
        .unwrap();
    assert_eq!(m.instrument_count(), 1);
    m.unsubscribe(vec![sub("SBIN", FeedMode::Ltp)])
        .await
        .unwrap();
    assert_eq!(m.instrument_count(), 0);
    // Unknown unsubscribe is a no-op.
    m.unsubscribe(vec![sub("TCS", FeedMode::Ltp)])
        .await
        .unwrap();
    wait_for(|| seen.lock().len() >= 6).await;
    let frames: Vec<String> = seen.lock().iter().map(|(_, f)| f.clone()).collect();
    assert_eq!(
        frames,
        [
            r#"{"sub":["NSE:SBIN:1"]}"#,
            r#"{"unsub":["NSE:SBIN:1"]}"#,
            r#"{"sub":["NSE:SBIN:2"]}"#,
            r#"{"unsub":["NSE:SBIN:2"]}"#,
            r#"{"sub":["NSE:SBIN:1"]}"#,
            r#"{"unsub":["NSE:SBIN:1"]}"#,
        ][..]
    );
    // Instrument cap.
    m.subscribe(vec![
        sub("A", FeedMode::Ltp),
        sub("B", FeedMode::Ltp),
        sub("C", FeedMode::Ltp),
    ])
    .await
    .unwrap();
    let e = m
        .subscribe(vec![sub("D", FeedMode::Ltp)])
        .await
        .unwrap_err();
    assert!(e.client_message().contains("at most 3 instruments"));
    m.unsubscribe_all().await.unwrap();
    assert_eq!(m.instrument_count(), 0);
    m.disconnect().await.unwrap();
    srv.abort();
}

#[tokio::test]
async fn stall_watchdog_reconnects_a_silent_socket() {
    let (url, _seen, srv) = server(|_| (vec![], false)).await;
    let m = WebSocketManager::with_config(FeedConfig {
        stall_timeout: Duration::from_millis(150),
        ..fast()
    });
    m.connect(Box::new(MockFeed::new(url))).await.unwrap();
    wait_for(|| m.stats().stalls.load(Ordering::Relaxed) >= 2).await;
    assert!(m.stats().connects.load(Ordering::Relaxed) >= 2);
    m.disconnect().await.unwrap();
    srv.abort();
}

#[tokio::test]
async fn handshake_refusal_stops_reconnecting() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let attempts = Arc::new(AtomicU64::new(0));
    let a2 = attempts.clone();
    let srv = tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            a2.fetch_add(1, Ordering::Relaxed);
            let _ = tokio_tungstenite::accept_hdr_async(tcp, |_: &Request, _: Response| {
                let mut resp = ErrorResponse::new(Some("forbidden".into()));
                *resp.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
                Err(resp)
            })
            .await;
        }
    });
    let m = WebSocketManager::with_config(fast());
    m.connect(Box::new(MockFeed::new(url))).await.unwrap();
    wait_for(|| matches!(m.status(), FeedStatus::AuthFailed { .. })).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    assert_eq!(m.stats().connects.load(Ordering::Relaxed), 0);
    match m.status() {
        FeedStatus::AuthFailed { message, .. } => {
            assert!(message.contains("Log in to your broker again"))
        }
        other => panic!("{:?}", other),
    }
    m.disconnect().await.unwrap();
    assert!(!m.is_running());
    srv.abort();
}

#[tokio::test]
async fn auth_failed_frame_stops_the_loop() {
    let (url, _seen, srv) = server(|_| (vec![r#"{"auth":"denied"}"#.to_string()], false)).await;
    let m = WebSocketManager::with_config(fast());
    m.connect(Box::new(MockFeed::new(url))).await.unwrap();
    wait_for(|| matches!(m.status(), FeedStatus::AuthFailed { .. })).await;
    assert_eq!(m.stats().connects.load(Ordering::Relaxed), 1);
    m.disconnect().await.unwrap();
    srv.abort();
}

#[tokio::test]
async fn unreachable_feed_backs_off_and_stops_cleanly() {
    // Bind then drop: nothing listens on this port any more.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let m = WebSocketManager::with_config(fast());
    let mut status = m.watch_status();
    m.connect(Box::new(MockFeed::new(format!("ws://127.0.0.1:{}", port))))
        .await
        .unwrap();
    let mut reconnects = 0;
    while reconnects < 3 {
        status.changed().await.unwrap();
        if let FeedStatus::Reconnecting { delay_ms, .. } = &*status.borrow() {
            assert!(*delay_ms <= 20);
            reconnects += 1;
        }
    }
    m.disconnect().await.unwrap();
    assert!(!m.is_running());
}

#[tokio::test]
async fn lagging_receivers_skip_ahead() {
    let ticks: Vec<String> = (0..200)
        .map(|i| format!(r#"{{"t":"SBIN","x":"NSE","p":{}}}"#, i))
        .collect();
    let (url, _seen, srv) = server(move |_| (ticks.clone(), false)).await;
    let m = WebSocketManager::with_config(FeedConfig {
        event_capacity: 8,
        ..fast()
    });
    let mut rx = m.subscribe_ticks();
    m.connect(Box::new(MockFeed::new(url))).await.unwrap();
    wait_for(|| m.stats().events.load(Ordering::Relaxed) >= 200).await;
    let mut lagged = false;
    let mut got = 0;
    loop {
        match rx.try_recv() {
            Ok(_) => got += 1,
            Err(broadcast::error::TryRecvError::Lagged(_)) => lagged = true,
            Err(_) => break,
        }
    }
    assert!(lagged);
    assert!(got <= 8);
    m.disconnect().await.unwrap();
    srv.abort();
}

/// A feed that needs async preparation, answers `PING` text frames with
/// `PONG`, and is accepted only by timeout (never acknowledged).
struct ProtocolFeed {
    base: String,
    prepares: Arc<AtomicU64>,
    url: Option<String>,
}

#[async_trait::async_trait]
impl BrokerFeed for ProtocolFeed {
    fn broker(&self) -> &'static str {
        "mock"
    }
    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        let n = self.prepares.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(5)).await;
        match n {
            // First attempt: the broker is unavailable; the manager backs off.
            0 => Err(PrepareError::Unavailable),
            // A single-use URL per connect.
            _ => {
                self.url = Some(format!("{}/?code={}", self.base, n));
                Ok(())
            }
        }
    }
    fn ws_request(&self) -> crate::error::Result<crate::brokers::common::streaming::WsRequest> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        self.url
            .as_deref()
            .ok_or_else(|| AppError::Internal("not prepared".into()))?
            .into_client_request()
            .map_err(|_| AppError::Internal("bad url".into()))
    }
    fn awaits_auth_ack(&self) -> bool {
        true
    }
    fn auth_ack_timeout(&self) -> Option<Duration> {
        Some(Duration::from_millis(150))
    }
    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        subs.iter()
            .map(|s| Message::Text(format!("SUB {}", s.symbol)))
            .collect()
    }
    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }
    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) if t == "PING" => vec![FeedEvent::Reply(Message::Text("PONG".into()))],
            _ => Vec::new(),
        }
    }
}

#[tokio::test]
async fn prepare_runs_before_connect_and_replies_go_back() {
    let (url, seen, srv) = server(|_| (vec!["PING".to_string()], false)).await;
    let m = WebSocketManager::with_config(fast());
    m.subscribe(vec![sub("SBIN", FeedMode::Ltp)]).await.unwrap();
    let prepares = Arc::new(AtomicU64::new(0));
    m.connect(Box::new(ProtocolFeed {
        base: url,
        prepares: prepares.clone(),
        url: None,
    }))
    .await
    .unwrap();
    // The reply reaches the broker; the subscribe goes out only after the
    // acknowledgement timeout, on the connection opened after a retry.
    wait_for(|| seen.lock().iter().any(|(_, t)| t == "PONG")).await;
    wait_for(|| seen.lock().iter().any(|(_, t)| t == "SUB SBIN")).await;
    assert!(m.is_connected());
    assert_eq!(prepares.load(Ordering::SeqCst), 2);
    let frames = seen.lock().clone();
    let pong = frames.iter().position(|(_, t)| t == "PONG").unwrap();
    let sub_at = frames.iter().position(|(_, t)| t == "SUB SBIN").unwrap();
    assert!(pong < sub_at, "{:?}", frames);
    m.disconnect().await.unwrap();
    srv.abort();
}

#[tokio::test]
async fn prepare_refusal_stops_until_login() {
    struct Refused;
    #[async_trait::async_trait]
    impl BrokerFeed for Refused {
        fn broker(&self) -> &'static str {
            "mock"
        }
        async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
            Err(PrepareError::AuthFailed("Log in again.".into()))
        }
        fn ws_request(&self) -> crate::error::Result<crate::brokers::common::streaming::WsRequest> {
            Err(AppError::Internal("unreachable".into()))
        }
        fn subscribe_frames(&mut self, _: &[FeedSubscription]) -> Vec<Message> {
            Vec::new()
        }
        fn unsubscribe_frames(&mut self, _: &[FeedSubscription]) -> Vec<Message> {
            Vec::new()
        }
        fn parse(&mut self, _: &Message) -> Vec<FeedEvent> {
            Vec::new()
        }
    }
    let m = WebSocketManager::with_config(fast());
    let mut status = m.watch_status();
    m.connect(Box::new(Refused)).await.unwrap();
    loop {
        status.changed().await.unwrap();
        if let FeedStatus::AuthFailed { message, .. } = &*status.borrow() {
            assert_eq!(message, "Log in again.");
            break;
        }
    }
    m.disconnect().await.unwrap();
    assert!(!m.is_running());
}

#[tokio::test]
async fn a_url_without_a_path_is_requested_at_the_root() {
    // Kite's and Dhan's feed URLs are `wss://host?query`; the request line
    // must be `GET /?query`, not `GET ?query`.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Option<String>>> = Arc::default();
    let seen2 = seen.clone();
    let srv = tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let seen = seen2.clone();
            let cb = move |req: &Request,
                           resp: Response|
                  -> std::result::Result<Response, ErrorResponse> {
                *seen.lock() = req.uri().path_and_query().map(|p| p.to_string());
                Ok(resp)
            };
            if let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await {
                while let Some(Ok(_)) = ws.next().await {}
            }
        }
    });
    let m = WebSocketManager::with_config(fast());
    m.connect(Box::new(MockFeed::new(format!("ws://{}?api_key=k", addr))))
        .await
        .unwrap();
    wait_for(|| m.is_connected()).await;
    assert_eq!(seen.lock().as_deref(), Some("/?api_key=k"));
    m.disconnect().await.unwrap();
    srv.abort();
}
