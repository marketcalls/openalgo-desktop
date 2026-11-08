//! What the feed does when Sahi ends a connection (web `sahi_websocket.py`
//! `_run`, audit P-12), answered through `BrokerFeed::on_ended`:
//!
//! * 4401 (token expired or revoked) and 4403 (forbidden): stop until the
//!   trader logs in again. The web reads the stored token again and stops
//!   when it has not changed; a desktop feed's token never changes under
//!   it (a new login starts a new feed), so that is at once.
//! * 4429 (more than five connections on the token): wait 60, 120, 240,
//!   300 and 300 s, then stop and tell the trader to close the other
//!   streaming apps; a connection that stayed up 30 s, or the trader
//!   reconnecting (`on_resume`), starts the count again.
//! * An upgrade refused with 429 (5 a second, 250 a minute per user): wait
//!   30 s, the web's backoff cap.
//! * Anything else (1000, 4408 ping timeout, 4409 slow consumer, a read
//!   error): the manager's capped backoff.

use super::is_auth_close;
use crate::brokers::common::streaming::{Ended, Reconnect, StopKind};
use std::time::Duration;
use tokio::time::Instant;

/// Sahi's close code for more than five market data connections on one
/// token (web `CLOSE_TOO_MANY_CONNECTIONS`).
const TOO_MANY_CONNECTIONS: u16 = 4429;
/// The status of an upgrade over Sahi's rate limit.
const UPGRADE_RATE_LIMITED: u16 = 429;
/// The web's waits after a 4429 (`LIMIT_BACKOFF_BASE`, `LIMIT_BACKOFF_CAP`,
/// `MAX_LIMIT_RETRIES`).
const LIMIT_WAIT_FIRST: Duration = Duration::from_secs(60);
const LIMIT_WAIT_CAP: Duration = Duration::from_secs(300);
const LIMIT_RETRIES: u32 = 5;
/// The web waits its backoff cap after a rate-limited upgrade.
const RATE_LIMIT_WAIT: Duration = Duration::from_secs(30);
/// A connection up this long starts the 4429 count again (`STABLE_AFTER`).
const STABLE_AFTER: Duration = Duration::from_secs(30);

const SESSION_ENDED: &str =
    "Sahi ended the live data session for this login. Log in to Sahi again.";
const TOO_MANY: &str = "Sahi allows five live data connections per login, and other apps or \
     sessions hold them. Close other Sahi streaming apps or sessions, then reconnect: reload \
     the page, or reconnect in the app.";

/// The 4429 count and when the current connection opened.
#[derive(Debug, Default)]
pub(super) struct ClosePolicy {
    limit_retries: u32,
    opened_at: Option<Instant>,
}

impl ClosePolicy {
    /// A connection opened.
    pub(super) fn opened(&mut self) {
        self.opened_at = Some(Instant::now());
    }

    /// The trader asked to connect again after a stop that was not a login
    /// failure: the web's rebuilt client starts its counts at 0.
    pub(super) fn resume(&mut self) {
        self.limit_retries = 0;
    }

    pub(super) fn ended(&mut self, ended: Ended<'_>) -> Reconnect {
        let lived = self.opened_at.take().map(|t| t.elapsed());
        if lived.is_some_and(|l| l >= STABLE_AFTER) {
            self.limit_retries = 0;
        }
        match ended {
            Ended::Closed {
                code: Some(code), ..
            } if is_auth_close(code) => Reconnect::Stop {
                kind: StopKind::Auth,
                message: SESSION_ENDED.into(),
            },
            Ended::Closed {
                code: Some(TOO_MANY_CONNECTIONS),
                ..
            } => self.connection_limit(),
            Ended::Refused(UPGRADE_RATE_LIMITED) => Reconnect::After(RATE_LIMIT_WAIT),
            _ => Reconnect::Backoff,
        }
    }

    fn connection_limit(&mut self) -> Reconnect {
        if self.limit_retries >= LIMIT_RETRIES {
            return Reconnect::Stop {
                kind: StopKind::ConnectionLimit,
                message: TOO_MANY.into(),
            };
        }
        let wait = LIMIT_WAIT_FIRST
            .saturating_mul(1 << self.limit_retries)
            .min(LIMIT_WAIT_CAP);
        self.limit_retries += 1;
        Reconnect::After(wait)
    }
}

#[cfg(test)]
mod tests;
