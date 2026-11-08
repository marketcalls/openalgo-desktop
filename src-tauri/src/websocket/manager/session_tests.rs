//! A session's edges. How a socket ended is the feed's to judge (audit
//! P-12): a close code or a refused upgrade asks `BrokerFeed::on_ended`,
//! and the manager waits as long as the broker asked, or stays down with
//! the trader's message ("log in again" only for a login the broker
//! refused). And a change made before a session was sent its whole set is
//! not sent again (Kotak, run 42519352778).

use super::deadline_tests::{supervisor, ScriptedFeed, StuckSink};
use super::tests::{fast, wait_for};
use super::*;
use crate::brokers::common::streaming::{Ended, FeedEvent, FeedMode, Message, Reconnect, StopKind};
use crate::brokers::mock::MockFeed;
use crate::websocket::handshake;
use std::collections::VecDeque;
use std::task::Poll;
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
    /// The trader asked to connect again (`BrokerFeed::on_resume`).
    Resumed,
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
    fn on_resume(&mut self) {
        self.heard.lock().push(Heard::Resumed);
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
/// so, never "log in again", and no further connection until a feed
/// client signs in (the web rebuilds a disconnected adapter at a client's
/// authentication): then a fresh run, with no logout.
#[tokio::test]
async fn a_close_code_waits_as_the_feed_asks_then_stops_until_a_client_signs_in() {
    let (url, came) = closing_server(4429, "limit").await;
    let words = "Close other Sahi streaming apps or sessions, then reconnect.";
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
    let first = came.lock().clone();
    assert_eq!(first.len(), 2, "no connection after the stop");
    assert!(first[1] - first[0] >= Duration::from_millis(300));
    assert_eq!(
        *heard.lock(),
        vec![Heard::Closed(Some(4429), "limit".into()); 2]
    );

    // A feed client signs in: the feed is told, and connects again.
    // (The broker keeps closing it, and the feed now asks for the backoff,
    // so more connections follow.)
    m.resume();
    for _ in 0..400 {
        if came.lock().len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        came.lock().len() >= 3,
        "status {:?}, heard {:?}",
        m.status(),
        heard.lock()
    );
    assert_eq!(heard.lock()[2], Heard::Resumed);
    m.disconnect().await.unwrap();
}

/// A resume reaches only a connection the broker ended: a client signing
/// in never drops a feed that streams.
#[tokio::test]
async fn a_resume_leaves_a_streaming_feed_alone() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let came: Arc<Mutex<usize>> = Arc::default();
    let seen = came.clone();
    tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((tcp, _)) = listener.accept().await {
            *seen.lock() += 1;
            if let Ok(ws) = tokio_tungstenite::accept_async(tcp).await {
                open.push(ws);
            }
        }
    });
    let (feed, heard) = judging(url, Vec::new());
    let m = WebSocketManager::with_config(fast());
    m.connect(feed).await.unwrap();
    wait_for(|| matches!(m.status(), FeedStatus::Connected { .. })).await;
    m.resume();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(*came.lock(), 1);
    assert!(heard.lock().is_empty());
    assert!(matches!(m.status(), FeedStatus::Connected { .. }));
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
    // A dead login is not resumed by a client signing in, nor by a wake.
    m.resume();
    m.reconnect();
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

fn infy(token: &str, brsymbol: &str) -> FeedSubscription {
    FeedSubscription {
        symbol: "INFY".into(),
        exchange: "NSE".into(),
        token: token.into(),
        brsymbol: brsymbol.into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Ltp,
        depth: 5,
    }
}

/// A feed that waits for its login acknowledgement, that
/// acknowledgement as the broker sends it, the instrument subscribed
/// while it is awaited, and how the feed's frames name that instrument.
struct Awaiting {
    broker: &'static str,
    feed: Box<dyn BrokerFeed>,
    ack: Message,
    sub: FeedSubscription,
    named: &'static str,
}

/// A Fyers HSM token that has not expired (the session key is read from
/// the access token's claims).
fn hsm_token() -> crate::brokers::types::AuthToken {
    use base64::Engine;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(r#"{"hsm_key":"hsmkey123","exp":4102444800,"fy_id":"<USER_ID>"}"#);
    crate::brokers::types::AuthToken::new(format!("APPID-100:eyJhbGciOiJIUzI1NiJ9.{claims}.c2ln"))
}

fn awaiting() -> Vec<Awaiting> {
    use crate::brokers::common::symbols::SymbolResolver;
    use crate::brokers::{fyers, kotak};
    vec![
        Awaiting {
            broker: "scripted",
            feed: Box::new(ScriptedFeed {
                awaits_ack: true,
                optional_ack: None,
                reply_within: None,
            }),
            ack: Message::Text("OK".into()),
            sub: infy("2", "INFY"),
            named: "SUB INFY",
        },
        // Kotak's SFeed: the auth response is message code 1117.
        Awaiting {
            broker: "kotak",
            feed: Box::new(kotak::streaming::KotakFeed::new(
                "wss://sfeed.kotaksecurities.com/apifeed",
                "sid",
                "U".into(),
                SymbolResolver::new(),
            )),
            ack: Message::Text(r#"{"message_code":1117}"#.into()),
            sub: infy("1594", "INFY-EQ"),
            named: "nse_cm|1594",
        },
        // Fyers HSM (bk-a, run 42520481941): the binary auth response "K".
        Awaiting {
            broker: "fyers",
            feed: match fyers::streaming::HsmFeed::new(
                fyers::streaming::HSM_URL,
                &hsm_token(),
                SymbolResolver::new(),
            ) {
                Ok(f) => Box::new(f),
                Err(e) => panic!("HSM feed: {e}"),
            },
            ack: Message::Binary(vec![0, 6, 1, 1, 1, 0, 1, b'K']),
            sub: infy("10100000001594", "NSE:INFY-EQ"),
            named: "sf|nse_cm|1594",
        },
    ]
}

/// Kotak (run 42519352778) and Fyers HSM (run 42520481941): a
/// subscription made while the login waited for its acknowledgement
/// reached the broker twice, in the whole set sent after the
/// acknowledgement and again as the queued change. The broker's first
/// frame here is the acknowledgement, and reading it queues the change
/// first, as the manager makes one (registry, then command).
#[tokio::test]
async fn a_subscription_made_while_the_login_waits_reaches_the_broker_once() {
    for case in awaiting() {
        let (mut sup, cmd) = supervisor(case.feed);
        let mut change = Some((sup.registry.clone(), cmd.clone(), case.sub));
        let mut ack = Some(case.ack);
        let mut read = futures_util::stream::poll_fn(move |_| {
            if let Some((registry, tx, sub)) = change.take() {
                let (deltas, seq) = {
                    let mut reg = registry.lock();
                    let added = reg.add(sub, 0).into_iter().map(|(_, d)| d).collect();
                    (added, reg.changes)
                };
                let _ = tx.try_send(Command::Apply(Apply::plain(deltas, seq)));
            }
            match ack.take() {
                Some(m) => Poll::Ready(Some(Ok(m))),
                None => Poll::Pending,
            }
        });
        let sent: Arc<Mutex<Vec<(Duration, String)>>> = Arc::default();
        let mut write = StuckSink {
            sent: sent.clone(),
            take: 100,
            start: Instant::now(),
            pongs: None,
        };
        let stop = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = cmd.send(Command::Stop).await;
        });
        let end = tokio::time::timeout(
            Duration::from_secs(5),
            sup.session("mock", &mut write, &mut read),
        )
        .await;
        stop.abort();
        assert!(matches!(end, Ok(SessionEnd::Stopped)), "{}", case.broker);
        let times: usize = sent
            .lock()
            .iter()
            .map(|(_, f)| f.matches(case.named).count())
            .sum();
        assert_eq!(times, 1, "{}: {:?}", case.broker, sent.lock());
    }
}
