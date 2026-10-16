//! LOG-08 over HTTP/2: what a broker's HTTP/2 server answers to an order
//! decides whether it is refused or uncertain (RFC 9113 section 8.7).
//!
//! A local server that speaks HTTP/2 frame by frame gives each answer
//! exactly; the client is the shared broker client's settings, with HTTP/2
//! from the first byte instead of by ALPN (the server has no TLS).

use super::*;
use crate::brokers::common::http::{builder, read_json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const DATA: u8 = 0x0;
const HEADERS: u8 = 0x1;
const RST_STREAM: u8 = 0x3;
const SETTINGS: u8 = 0x4;
const GOAWAY: u8 = 0x7;
const END_STREAM: u8 = 0x1;
const ACK: u8 = 0x1;

const NO_ERROR: u32 = 0x0;
const INTERNAL_ERROR: u32 = 0x2;
const REFUSED_STREAM: u32 = 0x7;
const ENHANCE_YOUR_CALM: u32 = 0xb;

/// What the server does once an order request has fully arrived.
#[derive(Clone, Copy)]
enum Answer {
    /// Reset the order's stream with this error code.
    Reset(u32),
    /// Send GOAWAY with this error code, naming the order's stream as the
    /// last one processed (`covers`) or the one before it, then close.
    GoAway { covers: bool, code: u32 },
    /// Close the connection without a word.
    Close,
}

fn frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut f = (payload.len() as u32).to_be_bytes()[1..].to_vec();
    f.extend([kind, flags]);
    f.extend(stream.to_be_bytes());
    f.extend(payload);
    f
}

/// Send FIN, then read until the client closes too (or a second passes),
/// so the close reaches the client after every frame sent before it.
async fn close(mut s: TcpStream) {
    let _ = s.shutdown().await;
    let mut buf = [0u8; 1024];
    let drain = async { while matches!(s.read(&mut buf).await, Ok(n) if n > 0) {} };
    let _ = tokio::time::timeout(Duration::from_secs(1), drain).await;
}

async fn serve(mut s: TcpStream, answer: Answer, orders: Arc<AtomicUsize>) -> std::io::Result<()> {
    let mut preface = [0u8; 24];
    s.read_exact(&mut preface).await?;
    s.write_all(&frame(SETTINGS, 0, 0, &[])).await?;
    loop {
        let mut head = [0u8; 9];
        s.read_exact(&mut head).await?;
        let len = u32::from_be_bytes([0, head[0], head[1], head[2]]) as usize;
        let (kind, flags) = (head[3], head[4]);
        let stream = u32::from_be_bytes([head[5], head[6], head[7], head[8]]) & 0x7fff_ffff;
        let mut payload = vec![0u8; len];
        s.read_exact(&mut payload).await?;
        if kind == SETTINGS && flags & ACK == 0 {
            s.write_all(&frame(SETTINGS, ACK, 0, &[])).await?;
        }
        if !matches!(kind, HEADERS | DATA) || flags & END_STREAM == 0 {
            continue;
        }
        // The whole order has arrived on `stream`.
        orders.fetch_add(1, Ordering::SeqCst);
        match answer {
            Answer::Reset(code) => {
                s.write_all(&frame(RST_STREAM, 0, stream, &code.to_be_bytes()))
                    .await?
            }
            Answer::GoAway { covers, code } => {
                let last = if covers { stream } else { stream - 1 };
                let mut payload = last.to_be_bytes().to_vec();
                payload.extend(code.to_be_bytes());
                s.write_all(&frame(GOAWAY, 0, 0, &payload)).await?;
                close(s).await;
                return Ok(());
            }
            Answer::Close => {
                close(s).await;
                return Ok(());
            }
        }
    }
}

/// A server that answers every order with `answer`, on every connection;
/// counts the orders that fully arrived.
async fn server(answer: Answer) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/orders", l.local_addr().unwrap());
    let orders = Arc::new(AtomicUsize::new(0));
    let seen = orders.clone();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            tokio::spawn(serve(s, answer, seen.clone()));
        }
    });
    (url, orders)
}

/// An adapter's placement over the shared client's settings.
async fn place(url: &str) -> Result<OrderResponse> {
    let client = builder()
        .http2_prior_knowledge()
        .no_proxy()
        .build()
        .unwrap();
    let resp = client
        .post(url)
        .json(&serde_json::json!({"symbol": "SBIN"}))
        .send()
        .await?;
    let (_, v): (_, serde_json::Value) = read_json("test", resp).await?;
    Ok(OrderResponse {
        order_id: v["orderid"].as_str().unwrap_or("X1").to_string(),
        message: None,
    })
}

async fn outcome(answer: Answer) -> (PlaceOutcome, usize) {
    let (url, orders) = server(answer).await;
    let r = placing(place(&url)).await;
    (classify(&r), orders.load(Ordering::SeqCst))
}

fn refused(o: &PlaceOutcome) -> bool {
    matches!(o, PlaceOutcome::Refused { .. })
}

#[tokio::test]
async fn a_refused_stream_is_retried_then_refused() {
    // REFUSED_STREAM: closed before any processing. reqwest sends it again
    // twice; a third refusal is the broker's definite "not placed".
    let (o, orders) = outcome(Answer::Reset(REFUSED_STREAM)).await;
    assert!(refused(&o), "{:?}", o);
    assert_eq!(orders, 3);
}

#[tokio::test]
async fn a_graceful_goaway_before_the_order_is_retried_then_refused() {
    let (o, orders) = outcome(Answer::GoAway {
        covers: false,
        code: NO_ERROR,
    })
    .await;
    assert!(refused(&o), "{:?}", o);
    // Retried on a new connection; a retry that met the closing one again
    // fails before it is sent, so not every retry reaches the server.
    assert!((1..=3).contains(&orders), "{}", orders);
}

#[tokio::test]
async fn a_goaway_with_an_error_before_the_order_is_refused_without_a_retry() {
    let (o, orders) = outcome(Answer::GoAway {
        covers: false,
        code: ENHANCE_YOUR_CALM,
    })
    .await;
    assert!(refused(&o), "{:?}", o);
    assert_eq!(orders, 1);
}

#[tokio::test]
async fn a_goaway_that_covers_the_order_is_uncertain_and_never_retried() {
    // The server may have processed the order and closed before answering.
    let (o, orders) = outcome(Answer::GoAway {
        covers: true,
        code: NO_ERROR,
    })
    .await;
    assert!(o.is_uncertain(), "{:?}", o);
    assert_eq!(orders, 1);
}

#[tokio::test]
async fn a_stream_reset_for_any_other_reason_is_uncertain() {
    let (o, orders) = outcome(Answer::Reset(INTERNAL_ERROR)).await;
    assert!(o.is_uncertain(), "{:?}", o);
    assert_eq!(orders, 1);
}

#[tokio::test]
async fn a_connection_closed_after_the_order_arrived_is_uncertain() {
    let (o, orders) = outcome(Answer::Close).await;
    assert!(o.is_uncertain(), "{:?}", o);
    assert_eq!(orders, 1);
}
