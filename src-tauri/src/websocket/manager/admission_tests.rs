//! Review R107-03: a broker that answers subscribes apart from the socket
//! (XTS over REST) is waited on, and what it refuses is not taken: each
//! refused item is answered with why and held by nobody, the others are
//! taken; the all-or-none subscribe fails and keeps nothing of the call.

use super::tests::{fast, server, sub, wait_for};
use super::*;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, Message, ReleaseAnswer};
use crate::brokers::mock::MockFeed;

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
