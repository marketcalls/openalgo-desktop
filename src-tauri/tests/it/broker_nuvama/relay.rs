//! The loopback line relay against a fake Nuvama streaming socket (plain
//! TCP, one JSON object per line): the ready frame, batched quote requests,
//! requests that go at once, lines both ways, keepalive answers as signs of
//! life, an over-long line, and nothing left open when the relay goes.

use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::relay;
use openalgo_desktop_lib::brokers::nuvama::tcp_relay::{self, Endpoint, LineRelay, MAX_LINE};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn fake_socket() -> (TcpListener, Endpoint) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    (
        l,
        Endpoint {
            host: "127.0.0.1".into(),
            port,
        },
    )
}

async fn next(ws: &mut Ws) -> Message {
    tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("a frame")
        .expect("open")
        .expect("ok")
}

fn quote(symbols: &[&str]) -> String {
    let s: Vec<_> = symbols
        .iter()
        .map(|x| serde_json::json!({"symbol": x}))
        .collect();
    serde_json::json!({"request": {"streaming_type": "quote", "data": {"accType": "EQ",
        "symbols": s}, "formFactor": "P", "appID": "A", "response_format": "json",
        "request_type": "subscribe"}, "echo": {}})
    .to_string()
}

#[tokio::test]
async fn the_relay_carries_lines_and_batches_quote_requests() {
    let (listener, endpoint) = fake_socket().await;
    let relay_side = LineRelay::start(endpoint).unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(relay_side.url())
        .await
        .unwrap();
    let (tcp, _) = listener.accept().await.unwrap();
    let (r, mut w) = tcp.into_split();
    let mut lines = BufReader::new(r).lines();
    assert_eq!(
        relay::control(next(&mut ws).await.to_text().unwrap()),
        Some(Ok(()))
    );

    // Two quote requests in a row: only the latest goes, after the quiet.
    let sent = Instant::now();
    ws.send(Message::Text(quote(&["1_NSE"]))).await.unwrap();
    ws.send(Message::Text(quote(&["1_NSE", "2_NSE"])))
        .await
        .unwrap();
    // The order stream's subscription is not batched: it goes first.
    let orders = r#"{"request":{"streaming_type":"orderFiler","request_type":"subscribe"}}"#;
    ws.send(Message::Text(orders.into())).await.unwrap();
    let first = lines.next_line().await.unwrap().unwrap();
    assert_eq!(first, orders);
    let second = lines.next_line().await.unwrap().unwrap();
    assert_eq!(second, quote(&["1_NSE", "2_NSE"]));
    assert!(
        sent.elapsed() >= Duration::from_millis(450),
        "{:?}",
        sent.elapsed()
    );

    // Nuvama's lines reach the feed; its `{}`, or part of a line, is
    // Nuvama's sign of life, apart from the relay's own (an empty pong).
    let packet = r#"{"response":{"streaming_type":"quote","data":{"sym":"1_NSE","ltp":"10"}}}"#;
    w.write_all(format!("{packet}\n{{}}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(next(&mut ws).await, Message::Text(packet.into()));
    w.write_all(b"{}\n").await.unwrap();
    let alive = next(&mut ws).await;
    assert!(tcp_relay::is_nuvama_alive(&alive), "{alive:?}");
    assert_ne!(alive, Message::Pong(Vec::new()), "not the relay's own sign");
    w.write_all(b"{\"resp").await.unwrap();
    assert!(tcp_relay::is_nuvama_alive(&next(&mut ws).await));
    w.write_all(b"onse\":{}}\n").await.unwrap();
    let line = loop {
        match next(&mut ws).await {
            m if tcp_relay::is_nuvama_alive(&m) => continue,
            m => break m,
        }
    };
    assert_eq!(line, Message::Text(r#"{"response":{}}"#.into()));

    // Dropping the relay closes Nuvama's socket.
    drop(ws);
    drop(relay_side);
    let end = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("the socket closes");
    assert!(matches!(end, Ok(None) | Err(_)));
}

/// web `MAX_PENDING_BYTES`: a partial line past 1 MiB ends the session.
#[tokio::test]
async fn a_line_without_end_ends_the_session() {
    let (listener, endpoint) = fake_socket().await;
    let relay_side = LineRelay::start(endpoint).unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(relay_side.url())
        .await
        .unwrap();
    let (mut tcp, _) = listener.accept().await.unwrap();
    assert!(relay::control(next(&mut ws).await.to_text().unwrap()).is_some());
    let junk = vec![b'x'; MAX_LINE + 4096];
    let _ = tcp.write_all(&junk).await;
    let end = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(end.is_ok(), "the feed's side is closed");
}

/// Nuvama unreachable: the feed's side closes, so the manager backs off.
#[tokio::test]
async fn an_unreachable_socket_closes_the_feed_side() {
    let (listener, endpoint) = fake_socket().await;
    drop(listener);
    let relay_side = LineRelay::start(endpoint).unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(relay_side.url())
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(20), ws.next())
        .await
        .expect("closed in time");
    assert!(!matches!(got, Some(Ok(Message::Text(_)))), "{got:?}");
}
