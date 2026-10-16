//! RATE-01: what one feed connection may send. Every frame is charged to the
//! connection's frame and byte budgets before it is parsed; a client that
//! spends either is closed with 1008. Real clients stay inside the default:
//! a 3000-instrument subscribe in one message, or one message per
//! instrument as an SDK loop sends them.

use crate::feed_support;

use feed_support::*;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::feed::server::FrameBudget;
use openalgo_desktop_lib::feed::FakeSource;
use serde_json::json;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// The close the budget sends (the outbox's 1008 for a client that stops
/// reading says "client too slow").
const CLOSED: Option<(u16, &str)> = Some((1008, "too many messages"));

/// Sends `frames` while reading the answers; returns the answers counted,
/// the close the server sent, if any, and the seconds it took.
async fn flood(url: &str, frames: Vec<Message>) -> (usize, Option<(u16, String)>, f64) {
    let c = Client::connect(url).await;
    let (mut tx, mut rx) = c.ws.split();
    let started = tokio::time::Instant::now();
    let sender = tokio::spawn(async move {
        for f in frames {
            if tx.send(f).await.is_err() {
                break;
            }
        }
    });
    let mut answers = 0usize;
    let mut close = None;
    while let Ok(Some(item)) = tokio::time::timeout(Duration::from_secs(10), rx.next()).await {
        match item {
            Ok(Message::Text(_)) => answers += 1,
            Ok(Message::Close(f)) => {
                close = f.map(|f| (u16::from(f.code), f.reason.to_string()));
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    sender.abort();
    (answers, close, elapsed)
}

/// 10,000 JSON pings from a client that never authenticated: answered up to
/// the budget (its burst plus what refilled meanwhile), then closed with
/// 1008. Before the budget every one was parsed and answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_flood_is_closed_after_its_budget() {
    let budget = FrameBudget {
        burst_frames: 100,
        frames_per_second: 50,
        ..FrameBudget::default()
    };
    let h = start_with(FakeSource::new(known()), |c| c.frame_budget = budget).await;
    let ping = Message::Text(json!({"action": "ping"}).to_string());
    let (answers, close, elapsed) = flood(&h.url, vec![ping; 10_000]).await;
    let close = close.as_ref().map(|(c, r)| (*c, r.as_str()));
    assert_eq!(close, CLOSED, "{} answers", answers);
    let allowed = 100.0 + 50.0 * elapsed + 1.0;
    assert!(
        answers as f64 <= allowed,
        "{} answers, at most {} allowed",
        answers,
        allowed
    );
    assert!(
        eventually(Duration::from_secs(5), || h.handle.stats().clients == 0).await,
        "the closed client is released"
    );
    h.handle.stop().await;
}

/// Large frames spend the byte budget even when they are few.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_frames_spend_the_byte_budget() {
    let budget = FrameBudget {
        burst_bytes: 64 * 1024,
        bytes_per_second: 16 * 1024,
        ..FrameBudget::default()
    };
    let h = start_with(FakeSource::new(known()), |c| c.frame_budget = budget).await;
    let big = json!({"action": "ping", "pad": "x".repeat(20 * 1024)}).to_string();
    let (answers, close, _) = flood(&h.url, vec![Message::Text(big); 50]).await;
    let close = close.as_ref().map(|(c, r)| (*c, r.as_str()));
    assert_eq!(close, CLOSED, "{} answers", answers);
    assert!(answers < 50, "{} answers", answers);
    h.handle.stop().await;
}

/// The default budget: an authenticated client subscribes 3000 instruments
/// in one message, then the same 3000 one message each, and every one is
/// acknowledged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_clients_stay_inside_the_default_budget() {
    let h = start_with(FakeSource::permissive(), |_| {}).await;
    let mut c = Client::connect(&h.url).await;
    c.auth().await;
    let symbols: Vec<_> = (0..3000)
        .map(|i| json!({"symbol": format!("SYM{}", i), "exchange": "NSE"}))
        .collect();
    let ack = c
        .request(json!({"action": "subscribe", "symbols": symbols, "mode": 1}))
        .await;
    assert_eq!(ack["status"], "success");
    assert_eq!(ack["subscriptions"].as_array().map(Vec::len), Some(3000));
    let v = c.request(json!({"action": "unsubscribe_all"})).await;
    assert_eq!(v["status"], "success");

    // One message per instrument, each answered before the next, as an
    // SDK's subscribe calls are.
    for i in 0..3000 {
        let v = c
            .request(json!({"action": "subscribe", "symbol": format!("SYM{}", i),
                            "exchange": "NSE", "mode": "LTP"}))
            .await;
        assert_eq!(v["status"], "success", "{}", v);
    }
    h.handle.stop().await;
}
