//! A session's edges. How a socket ended is the feed's to judge (audit
//! P-12): a close code or a refused upgrade asks `BrokerFeed::on_ended`,
//! and the manager waits as long as the broker asked, or stays down with
//! the trader's message ("log in again" only for a login the broker
//! refused).

use super::tests::{fast, wait_for};
use super::*;
use crate::brokers::common::streaming::{Ended, FeedEvent, Message, Reconnect, StopKind};
use crate::brokers::mock::MockFeed;
use crate::websocket::handshake;
use std::collections::VecDeque;
use tokio::net::TcpListener;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

/// How the feed heard a socket end.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Heard {
    Closed(Option<u16>, String),
    Refused(u16),
}

/// The mock feed, judging each end from a script (then the backoff) and
/// recording what it heard.
struct JudgingFeed {
    inner: MockFeed,
    answers: VecDeque<Reconnect>,
    heard: Arc<Mutex<Vec<Heard>>>,
}

impl BrokerFeed for JudgingFeed {
    fn broker(&self) -> &'static str {
        "mock"
    }
    fn ws_request(&self) -> crate::error::Result<crate::brokers::common::streaming::WsRequest> {
        self.inner.ws_request()
    }
    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.inner.subscribe_frames(subs)
    }
    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.inner.unsubscribe_frames(subs)
    }
    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        self.inner.parse(msg)
    }
    fn on_ended(&mut self, ended: Ended<'_>) -> Reconnect {
        self.heard.lock().push(match ended {
            Ended::Closed { code, reason } => Heard::Closed(code, reason.to_string()),
            Ended::Refused(status) => Heard::Refused(status),
        });
        self.answers.pop_front().unwrap_or(Reconnect::Backoff)
    }
}

fn judging(url: String, answers: Vec<Reconnect>) -> (Box<dyn BrokerFeed>, Arc<Mutex<Vec<Heard>>>) {
    let heard: Arc<Mutex<Vec<Heard>>> = Arc::default();
    let feed = JudgingFeed {
        inner: MockFeed::new(url),
        answers: answers.into(),
        heard: heard.clone(),
    };
    (Box::new(feed), heard)
}

/// A broker that closes every connection with `code` and `reason` once
/// it is open, recording when each one came.
async fn closing_server(code: u16, reason: &'static str) -> (String, Arc<Mutex<Vec<Instant>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let came: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let seen = came.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            seen.lock().push(Instant::now());
            if let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await {
                let _ = futures_util::SinkExt::send(
                    &mut ws,
                    Message::Close(Some(CloseFrame {
                        code: CloseCode::from(code),
                        reason: reason.into(),
                    })),
                )
                .await;
            }
        }
    });
    (url, came)
}

/// Sahi's 4429 (too many connections on one token, web
/// `sahi_websocket.py`): the wait the feed asks for, then a stop that says
/// so, never "log in again", and no further connection.
#[tokio::test]
async fn a_close_code_waits_as_the_feed_asks_then_stops_with_its_words() {
    let (url, came) = closing_server(4429, "limit").await;
    let words = "Close the other Sahi apps streaming on this login, then log in again.";
    let (feed, heard) = judging(
        url,
        vec![
            Reconnect::After(Duration::from_millis(300)),
            Reconnect::Stop {
                kind: StopKind::ConnectionLimit,
                message: words.into(),
            },
        ],
    );
    let m = WebSocketManager::with_config(fast());
    m.connect(feed).await.unwrap();
    wait_for(|| matches!(m.status(), FeedStatus::Reconnecting { delay_ms: 300, .. })).await;
    wait_for(|| matches!(m.status(), FeedStatus::Halted { .. })).await;
    assert_eq!(
        m.status(),
        FeedStatus::Halted {
            broker: "mock".into(),
            kind: StopKind::ConnectionLimit,
            message: words.into(),
        }
    );
    assert_eq!(m.status().refusal(), Some(words));
    assert_eq!(
        crate::services::feed_connections::state(&m.status()),
        "connection_limit"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    let came = came.lock().clone();
    assert_eq!(came.len(), 2, "no connection after the stop");
    assert!(came[1] - came[0] >= Duration::from_millis(300));
    assert_eq!(
        *heard.lock(),
        vec![Heard::Closed(Some(4429), "limit".into()); 2]
    );
    m.disconnect().await.unwrap();
}

/// A close the feed reads as a dead login is the login refusal, with the
/// feed's words.
#[tokio::test]
async fn a_close_code_the_feed_reads_as_a_dead_login_asks_for_a_login() {
    let (url, came) = closing_server(4401, "expired").await;
    let (feed, heard) = judging(
        url,
        vec![Reconnect::Stop {
            kind: StopKind::Auth,
            message: "Log in to Sahi again.".into(),
        }],
    );
    let m = WebSocketManager::with_config(fast());
    m.connect(feed).await.unwrap();
    wait_for(|| matches!(m.status(), FeedStatus::AuthFailed { .. })).await;
    assert_eq!(m.status().refusal(), Some("Log in to Sahi again."));
    assert_eq!(
        *heard.lock(),
        vec![Heard::Closed(Some(4401), "expired".into())]
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(came.lock().len(), 1);
    m.disconnect().await.unwrap();
}

/// An upgrade refused with a status the login check does not claim (HTTP
/// 429) is the feed's to judge; 401 stays the login refusal and is not
/// asked about.
#[tokio::test]
async fn a_refused_upgrade_is_the_feeds_to_judge_unless_it_is_the_login() {
    for (status, judged) in [(429u16, true), (401, false)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let srv = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let refuse = handshake::check(move |_: &Request, _: Response| {
                    let mut resp = ErrorResponse::new(Some("no".into()));
                    *resp.status_mut() =
                        tokio_tungstenite::tungstenite::http::StatusCode::from_u16(status).unwrap();
                    Err(Box::new(resp))
                });
                let _ = tokio_tungstenite::accept_hdr_async(tcp, refuse).await;
            }
        });
        let (feed, heard) = judging(
            url,
            vec![Reconnect::Stop {
                kind: StopKind::Other,
                message: "The broker is limiting new connections.".into(),
            }],
        );
        let m = WebSocketManager::with_config(fast());
        m.connect(feed).await.unwrap();
        wait_for(|| m.status().refusal().is_some()).await;
        if judged {
            assert_eq!(*heard.lock(), vec![Heard::Refused(429)]);
            assert!(matches!(
                m.status(),
                FeedStatus::Halted {
                    kind: StopKind::Other,
                    ..
                }
            ));
        } else {
            assert!(heard.lock().is_empty());
            assert!(matches!(m.status(), FeedStatus::AuthFailed { .. }));
        }
        m.disconnect().await.unwrap();
        srv.abort();
    }
}
