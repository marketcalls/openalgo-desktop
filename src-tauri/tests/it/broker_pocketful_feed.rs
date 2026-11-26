//! Pocketful's market and order feeds never carry the access token into a
//! log line (web test/test_pocketful_streaming.py::test_the_socket_signs_in_with_the_sessions_client_id_not_the_openalgo_user,
//! test/test_pocketful_order_updates.py::test_connect_subscribes_publishes_and_disconnect_stops_without_logging_the_token):
//! both sockets put the token in their address's query, so a connect that
//! fails, an upgrade refused and an update that cannot be read are each
//! logged without it. Every line at every level is captured and searched.

use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{
    FeedMode, FeedSubscription, Message, OrderFeed,
};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::pocketful::PocketfulBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::websocket::{handshake, FeedConfig, WebSocketManager};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

/// The access token's sentinel (no real credential).
const S: &str = "SENTINELq7Zx";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    /// Wait (bounded) until a line containing `what` was logged.
    async fn until(&self, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.text().contains(what) {
            assert!(
                Instant::now() < deadline,
                "never logged {:?}: {}",
                what,
                self.text()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// A Pocketful whose sockets answer the upgrade with 401, the token in the
/// body (as a server echoing the request might). The upgrade request is read
/// before the refusal goes out: a fake that answered without reading it and
/// closed made Windows reset the connection, so the client saw a reset and
/// never the 401.
async fn refusing() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((stream, _)) = l.accept().await {
            tokio::spawn(async move {
                let refuse = handshake::check(|_: &Request, _: Response| {
                    let mut resp = ErrorResponse::new(Some(format!("bad access_token={}", S)));
                    *resp.status_mut() = StatusCode::UNAUTHORIZED;
                    Err(Box::new(resp))
                });
                let _ = tokio_tungstenite::accept_hdr_async(stream, refuse).await;
            });
        }
    });
    url
}

/// A Pocketful that accepts the order socket and sends one order update
/// whose JSON cannot be read.
async fn sending_garbage() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((stream, _)) = l.accept().await {
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                let mut frame = vec![50u8, 0, 0, 4, 0x4c];
                frame.extend_from_slice(b"{not json");
                let _ = ws.send(Message::Binary(frame)).await;
                while let Some(Ok(_)) = ws.next().await {}
            });
        }
    });
    url
}

fn manager() -> WebSocketManager {
    WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        connect_timeout: Duration::from_millis(500),
        ..FeedConfig::default()
    })
}

fn sub(symbol: &str, exchange: &str, token: &str) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: exchange.into(),
        mode: FeedMode::Quote,
        depth: 5,
    }
}

/// Run the session's market and order feeds against `ws` until each of
/// `logged` has been logged, then stop both.
async fn run_feeds(ws: &str, logged: &[&str], log: &Captured) {
    let broker = PocketfulBroker::with_base_url(
        SymbolResolver::new(),
        "http://127.0.0.1:1",
        ws,
        "http://127.0.0.1:1",
    );
    let auth = AuthToken::new(S).with_user_id("C1");
    let market = manager();
    // An exchange the feed has no code for is logged, and never sent.
    market
        .subscribe(vec![
            sub("SBIN", "NSE", "3045"),
            sub("SOYBEAN", "NCDEX", "555"),
        ])
        .await
        .unwrap();
    let feed = broker.create_feed(&auth).expect("the market feed");
    let _ = market.connect(feed).await;
    let orders = manager();
    let Ok(OrderFeed::Socket(order_feed)) = broker.create_order_feed(&auth) else {
        panic!("the order feed is a socket of its own");
    };
    let _ = orders.connect(order_feed).await;
    for line in logged {
        log.until(line).await;
    }
    let _ = market.disconnect().await;
    let _ = orders.disconnect().await;
}

#[tokio::test]
async fn no_access_token_reaches_a_log() {
    // One thread: every task the managers spawn logs to this subscriber.
    let log = Captured::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let closed = format!("ws://127.0.0.1:{}", closed_port());
    run_feeds(&closed, &["reconnecting"], &log).await;
    run_feeds(&refusing().await, &["handshake refused"], &log).await;
    run_feeds(
        &sending_garbage().await,
        &[
            "Pocketful streams no live prices for NCDEX:SOYBEAN",
            "Could not read a Pocketful order update frame",
        ],
        &log,
    )
    .await;

    let text = log.text();
    assert!(
        !text.contains(S),
        "the access token reached a log: {}",
        text
    );
}
