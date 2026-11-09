//! Review R107-03: a broker that answers subscribes apart from the socket
//! (XTS over REST) is waited on, and what it refuses is not taken: each
//! refused item is answered with why and held by nobody, the others are
//! taken; the all-or-none subscribe fails and keeps nothing of the call.

use super::tests::{fast, server, sub, wait_for};
use super::*;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, Message, ReleaseAnswer};
use crate::brokers::mock::MockFeed;
use std::sync::atomic::Ordering;

/// The mock feed, answering its admissions itself: `BAD` refused.
struct RefusingFeed(MockFeed);

impl BrokerFeed for RefusingFeed {
    fn broker(&self) -> &'static str {
        "mock"
    }
    fn ws_request(&self) -> crate::error::Result<crate::brokers::common::streaming::WsRequest> {
        self.0.ws_request()
    }
    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.0.subscribe_frames(subs)
    }
    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.0.unsubscribe_frames(subs)
    }
    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        self.0.parse(msg)
    }
    fn admission_frames(
        &mut self,
        subs: &[FeedSubscription],
    ) -> (Vec<Message>, Option<ReleaseAnswer>) {
        let (to, answer) = tokio::sync::oneshot::channel();
        let refused = subs
            .iter()
            .filter(|s| s.symbol == "BAD")
            .map(|s| (s.clone(), "The broker refused BAD.".to_string()))
            .collect();
        let _ = to.send(refused);
        (self.0.subscribe_frames(subs), Some(answer))
    }
}

#[tokio::test]
async fn what_the_broker_refused_is_answered_and_not_held() {
    let (url, _seen, srv) = server(|_| (vec![], false)).await;
    let m = WebSocketManager::with_config(fast());
    m.connect(Box::new(RefusingFeed(MockFeed::new(url))))
        .await
        .unwrap();
    wait_for(|| m.is_connected()).await;

    let answers = m
        .admit(vec![sub("GOOD", FeedMode::Ltp), sub("BAD", FeedMode::Ltp)])
        .await;
    assert_eq!(
        answers,
        vec![Ok(()), Err("The broker refused BAD.".to_string())]
    );
    let held: Vec<String> = m.subscriptions().into_iter().map(|s| s.symbol).collect();
    assert_eq!(held, ["GOOD"]);

    // All or none: nothing of the call is kept.
    let all = m
        .subscribe(vec![sub("MORE", FeedMode::Ltp), sub("BAD", FeedMode::Ltp)])
        .await;
    assert!(
        matches!(&all, Err(AppError::Validation(why)) if why == "The broker refused BAD."),
        "{:?}",
        all
    );
    let held: Vec<String> = m.subscriptions().into_iter().map(|s| s.symbol).collect();
    assert_eq!(held, ["GOOD"]);
    m.disconnect().await.unwrap();
    srv.abort();
}

/// A feed whose broker takes each admission and never answers it.
struct UnansweredFeed {
    feed: MockFeed,
    held: Vec<tokio::sync::oneshot::Sender<crate::brokers::common::streaming::ReleaseRefusals>>,
}

impl BrokerFeed for UnansweredFeed {
    fn broker(&self) -> &'static str {
        "mock"
    }
    fn ws_request(&self) -> crate::error::Result<crate::brokers::common::streaming::WsRequest> {
        self.feed.ws_request()
    }
    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.feed.subscribe_frames(subs)
    }
    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.feed.unsubscribe_frames(subs)
    }
    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        self.feed.parse(msg)
    }
    fn admission_frames(
        &mut self,
        _subs: &[FeedSubscription],
    ) -> (Vec<Message>, Option<ReleaseAnswer>) {
        let (to, answer) = tokio::sync::oneshot::channel();
        self.held.push(to);
        (Vec::new(), Some(answer))
    }
}

/// Review X1: an admission the broker has not answered when the wait ends
/// is not taken for streaming: it is answered as not started and let go,
/// and the broker is told, should it take it after all.
#[tokio::test]
async fn an_answer_that_does_not_come_is_not_taken_for_streaming() {
    let (url, seen, srv) = server(|_| (vec![], false)).await;
    let m = WebSocketManager::with_config(FeedConfig {
        stall_timeout: Duration::from_secs(3600),
        ..fast()
    });
    let feed = UnansweredFeed {
        feed: MockFeed::new(url),
        held: Vec::new(),
    };
    m.connect(Box::new(feed)).await.unwrap();
    wait_for(|| m.is_connected()).await;

    tokio::time::pause();
    let answers = m.admit(vec![sub("X", FeedMode::Ltp)]).await;
    tokio::time::resume();
    assert!(
        matches!(answers.as_slice(), [Err(why)] if why.contains("did not confirm")),
        "{:?}",
        answers
    );
    assert!(m.subscriptions().is_empty(), "not held");
    wait_for(|| {
        seen.lock()
            .iter()
            .any(|(_, f)| f.contains("unsub") && f.contains(":X:"))
    })
    .await;
    m.disconnect().await.unwrap();
    srv.abort();
}

/// Waits inside the subscribe of its whole set, the moment the session
/// has read the registry, until the test lets it go on.
struct PausedAtTheRead {
    feed: MockFeed,
    at_the_read: Arc<std::sync::atomic::AtomicBool>,
    go_on: Arc<std::sync::atomic::AtomicBool>,
}

impl BrokerFeed for PausedAtTheRead {
    fn broker(&self) -> &'static str {
        "mock"
    }
    fn ws_request(&self) -> crate::error::Result<crate::brokers::common::streaming::WsRequest> {
        self.feed.ws_request()
    }
    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        if subs.iter().any(|s| s.symbol == "FIRST") {
            self.at_the_read.store(true, Ordering::SeqCst);
            for _ in 0..2500 {
                if self.go_on.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        self.feed.subscribe_frames(subs)
    }
    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.feed.unsubscribe_frames(subs)
    }
    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        self.feed.parse(msg)
    }
}

/// Review X3: a subscribe made after a new session read the registry and
/// before it reported itself connected reaches the broker; it was answered
/// and sent to nobody until the next reconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscribe_made_while_the_session_sends_its_set_reaches_the_broker() {
    let (url, seen, srv) = server(|_| (vec![], false)).await;
    let m = Arc::new(WebSocketManager::with_config(fast()));
    m.subscribe(vec![sub("FIRST", FeedMode::Ltp)])
        .await
        .unwrap();
    let at_the_read = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let go_on = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feed = PausedAtTheRead {
        feed: MockFeed::new(url),
        at_the_read: at_the_read.clone(),
        go_on: go_on.clone(),
    };
    m.connect(Box::new(feed)).await.unwrap();
    wait_for(|| at_the_read.load(Ordering::SeqCst)).await;

    let admitting = {
        let m = m.clone();
        tokio::spawn(async move { m.admit(vec![sub("LATE", FeedMode::Ltp)]).await })
    };
    wait_for(|| m.instrument_count() == 2).await;
    go_on.store(true, Ordering::SeqCst);

    assert_eq!(admitting.await.unwrap(), vec![Ok(())]);
    wait_for(|| {
        seen.lock()
            .iter()
            .any(|(_, f)| f.contains("sub") && f.contains(":LATE:"))
    })
    .await;
    m.disconnect().await.unwrap();
    srv.abort();
}
