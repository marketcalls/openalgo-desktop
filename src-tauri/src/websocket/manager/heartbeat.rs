//! A feed's client heartbeat on one connection: the frame it sends every
//! interval (`BrokerFeed::heartbeat`) and, for a feed that wants an answer
//! in time (`BrokerFeed::heartbeat_reply_timeout`), the deadline by which
//! some frame from the broker must follow it. The stall watchdog takes its
//! whole timeout to notice a half-open socket; an unanswered heartbeat
//! notices it sooner, as the web's websocket-client does with
//! `run_forever(ping_interval=.., ping_timeout=..)`.

use crate::brokers::common::streaming::{BrokerFeed, Message};
use std::pin::Pin;
use std::time::Duration;
use tokio::time::{Instant, Interval, Sleep};

/// Far enough away never to fire within a session.
const NEVER: Duration = Duration::from_secs(3600);

/// What the heartbeat asks of the session.
pub(super) enum Beat {
    /// Send this frame now.
    Due(Message),
    /// The broker did not answer the last heartbeat in time.
    Unanswered,
}

pub(super) struct Heartbeat {
    frame: Option<Message>,
    interval: Interval,
    reply_within: Option<Duration>,
    deadline: Pin<Box<Sleep>>,
    awaiting: bool,
}

impl Heartbeat {
    /// The heartbeat of a connection that has just opened: the first one
    /// is due one interval from now.
    pub(super) fn new(feed: &dyn BrokerFeed) -> Self {
        let (period, frame) = match feed.heartbeat() {
            Some((period, frame)) => (period, Some(frame)),
            None => (NEVER, None),
        };
        Self {
            frame,
            interval: tokio::time::interval_at(Instant::now() + period, period),
            reply_within: feed.heartbeat_reply_timeout(),
            deadline: Box::pin(tokio::time::sleep(NEVER)),
            awaiting: false,
        }
    }

    /// The next thing the session must do for the heartbeat; never, for a
    /// feed without one.
    pub(super) async fn next(&mut self) -> Beat {
        let Some(frame) = &self.frame else {
            return std::future::pending().await;
        };
        let awaiting = self.awaiting;
        tokio::select! {
            _ = self.deadline.as_mut(), if awaiting => Beat::Unanswered,
            _ = self.interval.tick() => Beat::Due(frame.clone()),
        }
    }

    /// The heartbeat went out: an answer is due within the reply timeout.
    /// One already awaited keeps its earlier deadline.
    pub(super) fn sent(&mut self) {
        if let (Some(within), false) = (self.reply_within, self.awaiting) {
            self.deadline.as_mut().reset(Instant::now() + within);
            self.awaiting = true;
        }
    }

    /// A frame arrived from the broker: it is there.
    pub(super) fn heard(&mut self) {
        self.awaiting = false;
    }
}
