//! The bot's Rate Limit setting (`bot_config.rate_limit_per_minute`,
//! "Maximum messages per minute per user"), applied to what OpenAlgo sends
//! one Telegram user through `TelegramService::send_alert`: order alerts,
//! `/api/v1/telegram/notify` and the Telegram page's messages. The web
//! stores the setting and never reads it (docs-46); applying it is a
//! deliberate deviation.
//!
//! A message over the limit waits for its turn rather than being dropped,
//! so a burst of alerts arrives late but complete and in order. One whose
//! turn is more than a window away is not sent: the sender keeps it in the
//! notification queue, as for a failed send. Replies to bot commands keep
//! their own caps (`messaging::ReplyLimiter`).

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tokio::time::Instant;

/// Chats tracked at most. Chats with nothing sent in the last window are
/// forgotten first; past the bound a message goes out untracked rather
/// than growing the table.
const MAX_CHATS: usize = 10_000;

/// When a message may go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Turn {
    Now,
    After(Duration),
    /// More than a window away: not booked.
    TooFar,
}

/// Each chat's booked send times, oldest first, still inside a window.
#[derive(Default)]
pub(crate) struct AlertPacer {
    chats: Mutex<HashMap<i64, VecDeque<Instant>>>,
}

impl AlertPacer {
    /// Book a send to `chat`, at most `per_window` sends in any `window`
    /// (a setting below one counts as one). Booked in turn order, so
    /// messages to a chat leave in the order they asked.
    pub(crate) fn book(&self, chat: i64, per_window: i64, window: Duration, now: Instant) -> Turn {
        let per_window = usize::try_from(per_window).unwrap_or(0).max(1);
        let mut chats = self.chats.lock();
        if !chats.contains_key(&chat) && chats.len() >= MAX_CHATS {
            chats.retain(|_, sends| sends.back().is_some_and(|t| *t + window > now));
            if chats.len() >= MAX_CHATS {
                return Turn::Now;
            }
        }
        let sends = chats.entry(chat).or_default();
        while sends.front().is_some_and(|t| *t + window <= now) {
            sends.pop_front();
        }
        let last = sends.back().copied().unwrap_or(now).max(now);
        // The send `per_window` places back must have left the window.
        let at = match sends.len().checked_sub(per_window) {
            Some(i) => (sends[i] + window).max(last),
            None => last,
        };
        let wait = at - now;
        if wait > window {
            return Turn::TooFar;
        }
        sends.push_back(at);
        if wait.is_zero() {
            Turn::Now
        } else {
            Turn::After(wait)
        }
    }

    #[cfg(test)]
    fn chats(&self) -> usize {
        self.chats.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn sends_within_the_limit_go_at_once_and_the_next_waits_for_the_window() {
        let p = AlertPacer::default();
        let t0 = Instant::now();
        assert_eq!(p.book(42, 2, MINUTE, t0), Turn::Now);
        assert_eq!(p.book(42, 2, MINUTE, t0 + secs(10)), Turn::Now);
        // The third waits until the first has left the minute.
        assert_eq!(p.book(42, 2, MINUTE, t0 + secs(20)), Turn::After(secs(40)));
        // Another chat has its own limit.
        assert_eq!(p.book(7, 2, MINUTE, t0 + secs(20)), Turn::Now);
    }

    #[test]
    fn a_burst_waits_in_order_and_what_cannot_go_within_a_window_is_refused() {
        let p = AlertPacer::default();
        let t0 = Instant::now();
        let turns: Vec<Turn> = (0..5).map(|_| p.book(42, 2, MINUTE, t0)).collect();
        assert_eq!(
            turns,
            vec![
                Turn::Now,
                Turn::Now,
                Turn::After(secs(60)),
                Turn::After(secs(60)),
                Turn::TooFar,
            ]
        );
        // Senders that waited their turn (the alert subscriber sends one at
        // a time) are never refused: each waits at most one window.
        assert_eq!(p.book(42, 2, MINUTE, t0 + secs(60)), Turn::After(secs(60)));
    }

    #[test]
    fn a_setting_below_one_counts_as_one() {
        let p = AlertPacer::default();
        let t0 = Instant::now();
        assert_eq!(p.book(42, 0, MINUTE, t0), Turn::Now);
        assert_eq!(p.book(42, -5, MINUTE, t0), Turn::After(MINUTE));
    }

    #[test]
    fn idle_chats_are_forgotten_when_the_table_is_full() {
        let p = AlertPacer::default();
        let t0 = Instant::now();
        for chat in 0..MAX_CHATS as i64 {
            p.book(chat, 30, MINUTE, t0);
        }
        assert_eq!(p.chats(), MAX_CHATS);
        // A minute on, every chat is idle: a new one makes room.
        assert_eq!(p.book(-1, 30, MINUTE, t0 + MINUTE), Turn::Now);
        assert_eq!(p.chats(), 1);
    }
}
