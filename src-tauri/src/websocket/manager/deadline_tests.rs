//! Session deadlines on a fake socket under a paused clock: a send the
//! peer never takes (TIME-02) and a login the broker never confirms
//! (TIME-05) both end the session, so the manager reconnects.

use super::*;
use crate::brokers::common::streaming::AUTH_ACK_DEADLINE;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Takes `take` frames, then never takes another: `poll_ready` stays
/// pending, as a send does once a peer stops reading and the socket
/// buffer is full.
struct StuckSink {
    sent: Arc<Mutex<Vec<(Duration, String)>>>,
    take: usize,
    start: Instant,
}

impl futures_util::Sink<Message> for StuckSink {
    type Error = WsError;

    fn poll_ready(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), WsError>> {
        if self.sent.lock().len() < self.take {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> std::result::Result<(), WsError> {
        let text = item.into_text().unwrap_or_default().to_string();
        self.sent.lock().push((self.start.elapsed(), text));
        Ok(())
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), WsError>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), WsError>> {
        Poll::Ready(Ok(()))
    }
}

type Frames =
    Pin<Box<dyn futures_util::Stream<Item = std::result::Result<Message, WsError>> + Send>>;

/// The broker side: frames the test sends arrive as reads; the stream
/// stays open while the sender lives.
fn broker_frames() -> (mpsc::Sender<Message>, Frames) {
    let (tx, rx) = mpsc::channel::<Message>(16);
    let stream =
        futures_util::stream::unfold(
            rx,
            |mut rx| async move { rx.recv().await.map(|m| (Ok(m), rx)) },
        );
    (tx, Box::pin(stream))
}

/// A feed that logs in with `LOGIN`, heartbeats with `HB` every 5 s and
/// answers `PING` with `PONG`. `HB` from the broker is a heartbeat and
/// `OK` its login acknowledgement.
struct ScriptedFeed {
    awaits_ack: bool,
    optional_ack: Option<Duration>,
}

impl BrokerFeed for ScriptedFeed {
    fn broker(&self) -> &'static str {
        "mock"
    }
    fn ws_request(&self) -> crate::error::Result<crate::brokers::common::streaming::WsRequest> {
        Err(AppError::Internal("not used".into()))
    }
    fn on_connected(&mut self) -> Vec<Message> {
        vec![Message::Text("LOGIN".into())]
    }
    fn awaits_auth_ack(&self) -> bool {
        self.awaits_ack
    }
    fn auth_ack_timeout(&self) -> Option<Duration> {
        self.optional_ack
    }
    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        subs.iter()
            .map(|s| Message::Text(format!("SUB {}", s.symbol)))
            .collect()
    }
    fn unsubscribe_frames(&mut self, _: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }
    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((Duration::from_secs(5), Message::Text("HB".into())))
    }
    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) if t.as_str() == "PING" => {
                vec![FeedEvent::Reply(Message::Text("PONG".into()))]
            }
            Message::Text(t) if t.as_str() == "OK" => vec![FeedEvent::AuthOk],
            _ => vec![FeedEvent::Heartbeat],
        }
    }
}

/// A supervisor for `feed` holding one subscription, and the sender of
/// its commands.
fn supervisor(feed: ScriptedFeed) -> (Supervisor, mpsc::Sender<Command>) {
    let (cmd_tx, cmd) = mpsc::channel(4);
    let sub = FeedSubscription {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        token: "1".into(),
        brsymbol: "SBIN".into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Ltp,
        depth: 5,
    };
    let mut registry = Registry::default();
    registry.entries.insert(
        sub.instrument_key(),
        Entry {
            base: sub,
            modes: BTreeMap::from([(FeedMode::Ltp, 1)]),
        },
    );
    let config = FeedConfig {
        stall_timeout: Duration::from_secs(60),
        write_timeout: Duration::from_secs(2),
        ..FeedConfig::default()
    };
    let s = Supervisor {
        feed: Box::new(feed),
        cmd,
        registry: Arc::new(Mutex::new(registry)),
        events: broadcast::channel(4).0,
        status: watch::channel(FeedStatus::Disconnected).0,
        config,
        stats: Arc::default(),
    };
    (s, cmd_tx)
}

/// How a session ended, when, and what reached the socket.
struct Run {
    end: Option<&'static str>,
    at: Duration,
    sent: Vec<(Duration, String)>,
}

/// Run one session on a socket that takes `take` frames; the broker sends
/// `script` (at, frame) and the manager stops it at `stop_at`.
async fn run_session(
    feed: ScriptedFeed,
    take: usize,
    script: Vec<(u64, &'static str)>,
    stop_at: Option<u64>,
) -> Run {
    let start = Instant::now();
    let (mut sup, cmd) = supervisor(feed);
    let sent: Arc<Mutex<Vec<(Duration, String)>>> = Arc::default();
    let mut write = StuckSink {
        sent: sent.clone(),
        take,
        start,
    };
    let (broker, mut read) = broker_frames();
    let driver = tokio::spawn(async move {
        for (at, frame) in script {
            tokio::time::sleep_until(start + Duration::from_secs(at)).await;
            let _ = broker.send(Message::Text(frame.into())).await;
        }
        if let Some(at) = stop_at {
            tokio::time::sleep_until(start + Duration::from_secs(at)).await;
            let _ = cmd.send(Command::Stop).await;
        }
        // Keep the socket open until the session is over.
        std::future::pending::<()>().await;
    });
    let ended = tokio::time::timeout(
        Duration::from_secs(120),
        sup.session("mock", &mut write, &mut read),
    )
    .await;
    driver.abort();
    let end = ended.ok().map(|e| match e {
        SessionEnd::Stopped => "stopped",
        SessionEnd::Lost => "lost",
        SessionEnd::Stalled => "stalled",
        SessionEnd::AuthFailed(_) => "auth_failed",
    });
    let sent = sent.lock().clone();
    Run {
        end,
        at: start.elapsed(),
        sent,
    }
}

fn plain() -> ScriptedFeed {
    ScriptedFeed {
        awaits_ack: false,
        optional_ack: None,
    }
}

/// Frames taken before the socket stops, the broker's frames, the last
/// frame sent, and the latest the session may end (seconds).
type Case = (usize, Vec<(u64, &'static str)>, Option<&'static str>, u64);

/// TIME-02: a send the peer never takes ends the session within the write
/// timeout, on connect, on re-subscribe, for a protocol reply and for the
/// heartbeat, long before the 60 s stall watchdog would.
#[tokio::test(start_paused = true)]
async fn a_send_the_peer_never_takes_ends_the_session() {
    let cases: [Case; 4] = [
        (0, vec![], None, 2),
        (1, vec![], Some("LOGIN"), 2),
        (2, vec![(1, "PING")], Some("SUB SBIN"), 3),
        (2, vec![], Some("SUB SBIN"), 7),
    ];
    for (i, (take, script, last, by)) in cases.into_iter().enumerate() {
        let run = run_session(plain(), take, script, None).await;
        assert_eq!(run.end, Some("lost"), "case {}: the session must end", i);
        assert!(
            run.at <= Duration::from_secs(by) + Duration::from_millis(50),
            "case {}: ended at {:?}, after {} s",
            i,
            run.at,
            by
        );
        assert_eq!(run.sent.last().map(|(_, f)| f.as_str()), last, "case {}", i);
    }
}

/// TIME-05: heartbeats with no login acknowledgement end the session at
/// the required deadline, with nothing subscribed.
#[tokio::test(start_paused = true)]
async fn a_login_never_confirmed_reconnects_at_the_deadline() {
    let feed = ScriptedFeed {
        awaits_ack: true,
        optional_ack: None,
    };
    let beats = (1..=8).map(|n| (n * 5, "HB")).collect();
    let run = run_session(feed, usize::MAX, beats, None).await;
    assert_eq!(run.end, Some("lost"));
    assert!(
        run.at >= AUTH_ACK_DEADLINE && run.at <= AUTH_ACK_DEADLINE + Duration::from_millis(50),
        "ended at {:?}",
        run.at
    );
    assert!(
        !run.sent.iter().any(|(_, f)| f.starts_with("SUB")),
        "nothing is subscribed before the login is confirmed: {:?}",
        run.sent
    );
}

/// A confirmation inside the deadline keeps the session; Groww's optional
/// acknowledgement still subscribes after its own wait.
#[tokio::test(start_paused = true)]
async fn a_late_confirmation_and_an_optional_one_keep_the_session() {
    let required = ScriptedFeed {
        awaits_ack: true,
        optional_ack: None,
    };
    let run = run_session(required, usize::MAX, vec![(15, "OK")], Some(30)).await;
    assert_eq!(run.end, Some("stopped"));
    let sub = run.sent.iter().find(|(_, f)| f == "SUB SBIN");
    assert_eq!(sub.map(|(at, _)| at.as_secs()), Some(15), "{:?}", run.sent);

    let optional = ScriptedFeed {
        awaits_ack: true,
        optional_ack: Some(Duration::from_secs(2)),
    };
    let run = run_session(optional, usize::MAX, vec![], Some(30)).await;
    assert_eq!(run.end, Some("stopped"));
    let sub = run.sent.iter().find(|(_, f)| f == "SUB SBIN");
    assert_eq!(sub.map(|(at, _)| at.as_secs()), Some(2), "{:?}", run.sent);
}
