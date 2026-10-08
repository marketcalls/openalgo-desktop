//! What the server has for one runner page: new bars, order frames and the
//! stop instruction, read by the page with a long poll.
//!
//! **Bounded by construction.** Bars are cumulative (a later answer restates
//! the bars an earlier one carried), so at most one bars message is pending;
//! an order frame restates the whole life of one order, so at most one frame
//! per order is pending. Anything else is a control message, of which a run
//! has a handful. A hard cap drops the oldest frame past [`MAX_PENDING`] as a
//! last resort. A message leaves once the page has acknowledged it by asking
//! for what comes after it.

use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::time::Duration;
use tokio::sync::Notify;

/// The most messages held for one page.
pub const MAX_PENDING: usize = 1000;

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Bars at or after the page's anchor, oldest first, `time` in ms.
    Bars(Vec<Value>),
    /// One order frame, keyed by the intent it answers.
    Frame { intent_id: i64, frame: Value },
    /// Stop sending orders: the run is being closed or paused.
    Halt,
}

impl Message {
    fn json(&self, seq: u64) -> Value {
        match self {
            Message::Bars(bars) => json!({"seq": seq, "kind": "bars", "bars": bars}),
            Message::Frame { frame, .. } => json!({"seq": seq, "kind": "frame", "frame": frame}),
            Message::Halt => json!({"seq": seq, "kind": "halt"}),
        }
    }
}

#[derive(Default)]
struct State {
    seq: u64,
    items: VecDeque<(u64, Message)>,
    closed: bool,
}

#[derive(Default)]
pub struct Inbox {
    state: Mutex<State>,
    notify: Notify,
}

fn bar_time(b: &Value) -> i64 {
    b.get("time").and_then(Value::as_i64).unwrap_or(i64::MIN)
}

impl Inbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue one message, folding it into the one it supersedes.
    pub fn push(&self, message: Message) {
        let mut s = self.state.lock();
        if s.closed {
            return;
        }
        let message = match message {
            Message::Bars(mut bars) => {
                if let Some(at) = s
                    .items
                    .iter()
                    .position(|(_, m)| matches!(m, Message::Bars(_)))
                {
                    if let Some((_, Message::Bars(older))) = s.items.remove(at) {
                        let newer: std::collections::HashSet<i64> =
                            bars.iter().map(bar_time).collect();
                        let mut merged: Vec<Value> = older
                            .into_iter()
                            .filter(|b| !newer.contains(&bar_time(b)))
                            .collect();
                        merged.append(&mut bars);
                        merged.sort_by_key(bar_time);
                        bars = merged;
                    }
                }
                Message::Bars(bars)
            }
            Message::Frame { intent_id, frame } => {
                s.items.retain(|(_, m)| {
                    !matches!(m, Message::Frame { intent_id: other, .. } if *other == intent_id)
                });
                Message::Frame { intent_id, frame }
            }
            other => other,
        };
        s.seq += 1;
        let seq = s.seq;
        s.items.push_back((seq, message));
        while s.items.len() > MAX_PENDING {
            match s
                .items
                .iter()
                .position(|(_, m)| matches!(m, Message::Frame { .. }))
            {
                Some(at) => {
                    s.items.remove(at);
                }
                None => {
                    s.items.pop_front();
                }
            }
        }
        drop(s);
        self.notify.notify_waiters();
    }

    /// Close the inbox: every waiting and later read answers at once with
    /// `closed`, which tells the page to stop.
    pub fn close(&self) {
        self.state.lock().closed = true;
        self.notify.notify_waiters();
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    pub fn pending(&self) -> usize {
        self.state.lock().items.len()
    }

    /// Messages after `after`, waiting up to `wait` for one to arrive.
    /// Everything up to `after` is acknowledged and dropped.
    pub async fn next(&self, after: u64, wait: Duration) -> (Vec<Value>, bool) {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut s = self.state.lock();
                s.items.retain(|(seq, _)| *seq > after);
                if s.closed || !s.items.is_empty() {
                    let out = s.items.iter().map(|(seq, m)| m.json(*seq)).collect();
                    return (out, s.closed);
                }
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep_until(deadline) => return (Vec::new(), false),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(t: i64, c: f64) -> Value {
        json!({"time": t, "close": c})
    }

    #[tokio::test]
    async fn bars_and_frames_fold_into_what_they_supersede() {
        let inbox = Inbox::new();
        inbox.push(Message::Bars(vec![bar(1, 1.0), bar(2, 2.0)]));
        inbox.push(Message::Frame {
            intent_id: 7,
            frame: json!({"intentId": 7, "status": "working"}),
        });
        inbox.push(Message::Bars(vec![bar(2, 2.5), bar(3, 3.0)]));
        inbox.push(Message::Frame {
            intent_id: 7,
            frame: json!({"intentId": 7, "status": "filled"}),
        });
        assert_eq!(inbox.pending(), 2);
        let (got, closed) = inbox.next(0, Duration::from_millis(10)).await;
        assert!(!closed);
        assert_eq!(got[0]["kind"], "bars");
        assert_eq!(
            got[0]["bars"],
            json!([bar(1, 1.0), bar(2, 2.5), bar(3, 3.0)])
        );
        assert_eq!(got[1]["frame"]["status"], "filled");
        let last = got[1]["seq"].as_u64().unwrap();
        let (got, _) = inbox.next(last, Duration::from_millis(10)).await;
        assert!(got.is_empty());
        assert_eq!(inbox.pending(), 0);
    }

    #[tokio::test]
    async fn a_waiting_read_wakes_on_push_and_on_close() {
        let inbox = std::sync::Arc::new(Inbox::new());
        let reader = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.next(0, Duration::from_secs(5)).await })
        };
        tokio::task::yield_now().await;
        inbox.push(Message::Halt);
        let (got, _) = reader.await.unwrap();
        assert_eq!(got[0]["kind"], "halt");

        let reader = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.next(10, Duration::from_secs(5)).await })
        };
        tokio::task::yield_now().await;
        inbox.close();
        let (_, closed) = reader.await.unwrap();
        assert!(closed);
        inbox.push(Message::Halt);
        assert_eq!(inbox.pending(), 0);
    }

    #[tokio::test]
    async fn the_queue_is_capped() {
        let inbox = Inbox::new();
        for i in 0..(MAX_PENDING as i64 + 50) {
            inbox.push(Message::Frame {
                intent_id: i,
                frame: json!({}),
            });
        }
        assert_eq!(inbox.pending(), MAX_PENDING);
    }
}
