//! A waited release on a socket that stops taking frames (audit C01; web
//! test/test_websocket_unsubscribe_contract.py::test_shoonya_exact_unsubscribe_real_flush_failure_retains_every_owner_and_queue):
//! the Noren `u` frame does not go out, so the release answers that the
//! socket did not take it and the reference is held again before the
//! session ends; the next socket subscribes it again, and the retry there
//! releases it once.

use super::registry::Registry;
use super::release::{Written, RELEASE_NOT_TAKEN};
use super::supervisor::{SessionEnd, Supervisor};
use super::*;
use crate::brokers::common::streaming::{FeedMode, Message};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::streaming::NorenFeed;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_tungstenite::tungstenite::Error as WsError;

/// Takes `take` frames, then fails every send (the connection dropped).
struct Socket {
    sent: Arc<Mutex<Vec<String>>>,
    take: usize,
}

impl futures_util::Sink<Message> for Socket {
    type Error = WsError;
    fn poll_ready(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), WsError>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, item: Message) -> std::result::Result<(), WsError> {
        let me = self.get_mut();
        let mut sent = me.sent.lock();
        if sent.len() >= me.take {
            return Err(WsError::ConnectionClosed);
        }
        sent.push(match item {
            Message::Text(t) => t.to_string(),
            _ => String::new(),
        });
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

fn reliance() -> FeedSubscription {
    FeedSubscription {
        symbol: "RELIANCE".into(),
        exchange: "NSE".into(),
        token: "2885".into(),
        brsymbol: "RELIANCE-EQ".into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Ltp,
        depth: 5,
    }
}

fn noren() -> Box<dyn BrokerFeed> {
    Box::new(NorenFeed::new(
        &crate::brokers::shoonya::CONFIG,
        "ws://127.0.0.1:1",
        "UID1",
        "session-token",
        SymbolResolver::new(),
    ))
}

/// One session on a socket that takes `take` frames: the broker accepts
/// the login, then the session is handed the release. Returns how the
/// session ended, the subscription frames the socket took, and what the
/// waiting caller heard.
async fn session(
    registry: &Arc<Mutex<Registry>>,
    take: usize,
) -> (&'static str, Vec<String>, Option<Written>) {
    let (cmd_tx, cmd) = mpsc::channel(4);
    let mut sup = Supervisor {
        feed: noren(),
        cmd,
        registry: registry.clone(),
        events: broadcast::channel(4).0,
        status: watch::channel(FeedStatus::Disconnected).0,
        config: FeedConfig::default(),
        stats: Arc::default(),
        conn: 0,
    };
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut write = Socket {
        sent: sent.clone(),
        take,
    };
    let (broker, rx) = mpsc::channel::<Message>(4);
    let mut read = Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|m| (Ok::<_, WsError>(m), rx))
    }));
    let (reply, written) = oneshot::channel();
    let logged_in = sent.clone();
    let reg = registry.clone();
    let driver = tokio::spawn(async move {
        let _ = broker
            .send(Message::Text(r#"{"t":"ak","s":"OK"}"#.into()))
            .await;
        // The release comes once the login's frames went out: the login,
        // the order stream and the subscription.
        while logged_in.lock().len() < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The manager's release: the last reference goes, then the change.
        let Some((_, change)) = reg.lock().remove(&reliance()) else {
            return;
        };
        let _ = cmd_tx
            .send(Command::Apply(Apply {
                deltas: vec![change],
                reply: Some(reply),
                undo: vec![reliance()],
            }))
            .await;
        // Hold the socket open until the session is over.
        std::future::pending::<()>().await;
    });
    let ended = tokio::time::timeout(
        Duration::from_secs(5),
        sup.session("shoonya", &mut write, &mut read),
    )
    .await;
    driver.abort();
    let end = match ended {
        Ok(SessionEnd::Dropped(cause)) => cause.name(),
        Ok(SessionEnd::Stopped) => "stopped",
        Ok(SessionEnd::AuthFailed(_)) => "auth_failed",
        Err(_) => "still running",
    };
    let sent = sent
        .lock()
        .iter()
        .filter(|f| f.contains(r#""k":"#))
        .cloned()
        .collect();
    (end, sent, written.await.ok())
}

#[tokio::test]
async fn a_release_the_socket_did_not_take_is_held_again_and_its_retry_releases_it_once() {
    let registry: Arc<Mutex<Registry>> = Arc::default();
    registry.lock().add(reliance(), 0);

    // The login's subscription goes out; the `u` frame finds the socket gone.
    let (end, sent, heard) = session(&registry, 3).await;
    assert_eq!(end, "closed");
    assert_eq!(sent, vec![r#"{"t":"t","k":"NSE|2885"}"#.to_string()]);
    assert!(matches!(heard, Some(Written::Failed)));
    assert_eq!(
        registry.lock().effective_all(),
        vec![reliance()],
        "held again before the session ended"
    );
    assert!(RELEASE_NOT_TAKEN.contains("stays subscribed"));

    // The next socket subscribes it again; the retry releases it there.
    let (end, sent, heard) = session(&registry, 10).await;
    assert_eq!(end, "still running");
    assert_eq!(
        sent,
        vec![
            r#"{"t":"t","k":"NSE|2885"}"#.to_string(),
            r#"{"t":"u","k":"NSE|2885"}"#.to_string(),
        ]
    );
    assert!(matches!(heard, Some(Written::Sent(later)) if later.is_empty()));
    assert!(registry.lock().effective_all().is_empty());
}
