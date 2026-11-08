//! Why a broker sign-in failed, kept here for the Select Broker page.
//!
//! A sign-in that fails on a page load (the broker's redirect, the XTS
//! post-back, the start of an OAuth or OTP login) sends the browser back to
//! `/broker?error=<id>&broker=<broker>`. The address carries only a random
//! id, never the sentence: Select Broker shows text it reads from here, so a
//! link someone else built can make it show nothing but OpenAlgo's own
//! fixed sentence (the web shows fixed sentences by code since eec700046;
//! the desktop's reasons are the broker's own, so they stay server-side).
//!
//! A reason belongs to the browser session that came back with it and is
//! read once (`GET /api/broker/sign-in-failure/{id}`, signed in). One that
//! arrived without a session (the XTS brokers post back across sites, with
//! no cookie) is read once by any signed-in session; its id is as
//! unguessable as a session id. Bounded and expiring like every cache
//! (`crate::cache::TtlCache`), and cleared with the browser sessions at
//! logout.

use crate::cache::TtlCache;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

/// Reasons kept at once. A sign-in starts from the trader's own clicks, so
/// a handful at most are waiting; a burst of failed callbacks from the
/// network (limited per address) drops the ones closest to expiry.
pub(crate) const MAX_FAILURES: usize = 64;

/// Long enough for the page to load after the redirect, short enough that a
/// reason never shows for a later visit.
pub(crate) const FAILURE_TTL: Duration = Duration::from_secs(10 * 60);

/// One failed sign-in, as Select Broker shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SignInFailure {
    /// The broker the sign-in was for (a catalogue id, checked by the route).
    pub(crate) broker: String,
    /// The trader-facing reason (`AppError::client_message`).
    pub(crate) message: String,
}

#[derive(Clone)]
struct Kept {
    failure: SignInFailure,
    /// The browser session it came back with, if it had one.
    owner: Option<String>,
}

pub(crate) struct SignInFailures {
    kept: TtlCache<String, Kept>,
}

impl Default for SignInFailures {
    fn default() -> Self {
        Self {
            kept: TtlCache::new(MAX_FAILURES, FAILURE_TTL),
        }
    }
}

impl SignInFailures {
    /// Keep `failure` for the session `owner` and return the id the
    /// address carries. A logout racing this stores nothing; the page then
    /// shows the fixed sentence.
    pub(crate) fn record(
        &self,
        failure: SignInFailure,
        owner: Option<&str>,
        now: Instant,
    ) -> String {
        let id = super::web::random_token();
        let kept = Kept {
            failure,
            owner: owner.map(str::to_string),
        };
        let generation = self.kept.generation();
        self.kept.fill(id.clone(), kept, generation, now);
        id
    }

    /// The reason behind `id`, once, for the session `reader`: never one
    /// that came back with another browser session.
    pub(crate) fn take(&self, id: &str, reader: &str, now: Instant) -> Option<SignInFailure> {
        self.kept
            .take_if(&id.to_string(), now, |k| {
                k.owner.as_deref().is_none_or(|o| o == reader)
            })
            .map(|k| k.failure)
    }

    /// Drop every reason (logout, with the browser sessions).
    pub(crate) fn clear(&self) {
        self.kept.clear();
    }
}
