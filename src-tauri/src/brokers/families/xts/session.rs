//! The market-data session quotes and history renew, which 5paisa XTS's
//! feed connects with too (web fivepaisaxts #2484: one market-data session
//! for feed and history).
//!
//! XTS does not let an `/apibinarymarketdata` session and an
//! `/apimarketdata` session of one market-data app live side by side: the
//! newer login cancels the other, while several `/apimarketdata` sessions
//! coexist. A session renewed after an "Invalid Token" answer is kept here,
//! so the calls after it and the feed's next connect use it instead of
//! logging in again.
//!
//! A cache under the app's rules (`crate::cache::TtlCache`,
//! `docs/port/no-keychain.md`): one slot, since one broker session runs at a
//! time; kept until the next 03:00 IST, when XTS tokens end; a renewal is
//! kept only when nothing cleared the slot while it logged in (the
//! generation read before the login); every sign-in, resume and sign-out
//! clears it (`XtsBroker::remember_market_keys`, which `on_logout` runs at
//! a logout, a broker switch and the daily boundary). The token is a
//! `Secret`: never logged, never printed.

use crate::brokers::common::jwt;
use crate::cache::{until_session_boundary, TtlCache};
use crate::security::Secret;
use std::time::{Duration, Instant};

/// When XTS ends its tokens: the daily session boundary, 03:00 IST.
const TOKEN_EXPIRY_IST: (u32, u32) = (3, 0);

/// A market-data session: its token and the user id XTS gave with it.
#[derive(Clone)]
pub(crate) struct SharedSession {
    pub token: Secret,
    pub user_id: String,
}

/// The one session slot an adapter and its feed share.
pub(crate) struct MarketSessions {
    slot: TtlCache<(), SharedSession>,
}

impl Default for MarketSessions {
    fn default() -> Self {
        Self {
            slot: TtlCache::new(1, Duration::from_secs(24 * 3600)),
        }
    }
}

impl MarketSessions {
    /// The renewed session, while it lasts.
    pub(crate) fn current(&self) -> Option<SharedSession> {
        self.slot.get(&(), Instant::now())
    }

    /// Read it before a renewal's login, and hand it to `renewed`.
    pub(crate) fn generation(&self) -> u64 {
        self.slot.generation()
    }

    /// Keep a session renewed since `generation`, until the next token
    /// expiry. False when a sign-in, resume or sign-out cleared the slot
    /// meanwhile: the session belongs to the broker session that ended.
    pub(crate) fn renewed(&self, session: SharedSession, generation: u64) -> bool {
        let (hour, minute) = TOKEN_EXPIRY_IST;
        let ttl = until_session_boundary(chrono::Utc::now(), hour, minute);
        self.slot
            .fill_for((), session, generation, Instant::now(), ttl)
    }

    /// Forget the renewed session (after the change that ends it).
    pub(crate) fn clear(&self) {
        self.slot.clear();
    }
}

/// The user id the market-data socket address takes, read from the
/// session's token (web fivepaisaxts `market_data_user_id`): XTS's token is
/// a JWT whose `userID` claim reads `<user id>_<app id>`, and the address
/// takes the part before the `_`. `None` when the token is not such a JWT.
pub(crate) fn market_data_user_id(token: &str) -> Option<String> {
    let claim = jwt::text_claim(&jwt::claims(token)?, "userID")?;
    let user = claim.split('_').next().unwrap_or_default();
    (!user.is_empty()).then(|| user.to_string())
}
