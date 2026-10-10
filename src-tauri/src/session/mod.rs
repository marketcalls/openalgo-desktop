//! Sessions: the daily boundary, browser sessions, and the task that ends
//! everything at the boundary.

pub mod boundary;
pub mod web;

use crate::brokers::catalog::{self, SessionPolicy};
use crate::brokers::types::AuthToken;
use crate::services::broker_auth_service::BrokerAuthService;
use crate::services::master_contract_service;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Duration;

/// Poll interval of the expiry task. Short enough that a machine waking from
/// sleep past 03:00 expires the session within a minute, and long enough to
/// cost nothing.
pub const EXPIRY_POLL: Duration = Duration::from_secs(30);

/// One step of the expiry task: if a boundary passed after `last_check`
/// (read from the session clock, so a clock jumping backward cannot move
/// it back), end what was authenticated before it, by the server-recorded
/// sign-in time: the broker session and its stored row, and the app's
/// browser sessions (SES-02: a sign-in made after the boundary, before this
/// poll or right after a wake from sleep, stays). The app's sessions expire
/// at the boundary for every broker; only the broker session of a
/// continuous (crypto) broker is exempt (SES-01). Returns whether it fired.
pub async fn expire_if_crossed(ctx: &AppState, last_check: DateTime<Utc>) -> bool {
    let now = ctx.session_now();
    let boundary = ctx.session_boundary();
    if !(last_check < boundary && now >= boundary) {
        return false;
    }
    tracing::info!("Daily session boundary reached; ending sessions from before it");
    if let Err(e) = BrokerAuthService::expire_before(ctx, boundary).await {
        tracing::error!("Could not revoke the broker session at the boundary: {}", e);
    }
    ctx.sessions.end_before(boundary);
    true
}

/// A continuous (crypto) session is not renewed at the daily boundary, so
/// its master contract is refreshed when the UTC day turns between
/// `last_check` and now (Delta lists new expiries every day; its master
/// follows the UTC day): the smart rule then downloads once. Runs as a task
/// of the session, aborted with it. Returns whether a refresh started
/// (14-N2).
pub fn refresh_master_if_day_turned(ctx: &AppState, last_check: DateTime<Utc>) -> bool {
    let now = ctx.session_now();
    if last_check.date_naive() == now.date_naive() {
        return false;
    }
    let Some(s) = ctx.broker_session.read().clone() else {
        return false;
    };
    if catalog::session_policy(&s.broker_id) != SessionPolicy::Continuous {
        return false;
    }
    let (Some(broker), Some(me)) = (ctx.brokers.get(&s.broker_id), ctx.arc()) else {
        return false;
    };
    let auth = AuthToken::new(s.auth_token.expose())
        .with_feed(s.feed_token.as_ref().map(|t| t.expose().to_string()))
        .with_user_id(s.user_id.clone());
    let weak = Arc::downgrade(&me);
    drop(me);
    tracing::info!(
        "UTC day turned; refreshing the {} master contract",
        s.broker_id
    );
    ctx.runtime.spawn_task(async move {
        let Some(ctx) = weak.upgrade() else {
            return;
        };
        match master_contract_service::ensure(&ctx, &broker, &auth).await {
            Ok(_) => ctx.bridge.resync(),
            Err(e) => tracing::warn!("Master contract refresh failed: {}", e),
        }
    });
    true
}

/// Start the owned background task that enforces the daily boundary.
pub fn spawn_expiry_task(ctx: Arc<AppState>) {
    let token = ctx.shutdown.clone();
    // Weak: the context owns this task, so a strong reference would be a cycle.
    let weak = Arc::downgrade(&ctx);
    let mut last = ctx.session_now();
    ctx.spawn(async move {
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(EXPIRY_POLL) => {
                    let Some(c) = weak.upgrade() else { break };
                    expire_if_crossed(&c, last).await;
                    refresh_master_if_day_turned(&c, last);
                    last = c.session_now();
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::common::symbols::tests::row;
    use crate::brokers::mock::{MockBroker, MockCall};
    use crate::brokers::{Broker, BrokerRegistry};
    use crate::security::Secret;
    use crate::state::BrokerSession;
    use chrono::TimeZone;
    use chrono_tz::Asia::Kolkata;

    fn ist(d: u32, h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Kolkata
            .with_ymd_and_hms(2026, 10, d, h, m, s)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn harness(now: DateTime<Utc>) -> (crate::state::testing::TestCtx, Arc<MockBroker>) {
        let delta = Arc::new(MockBroker::new("deltaexchange"));
        let zerodha = Arc::new(MockBroker::new("zerodha"));
        *delta.master.lock() = Some(Ok(vec![row("BTCUSDFUT", "BTCUSD", "CRYPTO", "27")]));
        *zerodha.master.lock() = Some(Ok(vec![row("SBIN", "SBIN", "NSE", "779521")]));
        let t = crate::state::testing::build(
            BrokerRegistry::with(vec![
                delta.clone() as Arc<dyn Broker>,
                zerodha as Arc<dyn Broker>,
            ]),
            now,
        );
        (t, delta)
    }

    fn sign_in(ctx: &AppState, broker: &str) -> BrokerSession {
        let s = BrokerSession {
            broker_id: broker.into(),
            auth_token: Secret::new("key:secret"),
            feed_token: None,
            user_id: "U1".into(),
            user_name: None,
            authenticated_at: ctx.session_now(),
        };
        BrokerAuthService::persist(ctx, &s).unwrap();
        s
    }

    fn browser(ctx: &AppState) -> String {
        let s = ctx.sessions.create(ctx.now());
        let now = ctx.now();
        ctx.sessions.update(&s.id, |x| {
            x.user = Some("trader".into());
            x.authenticated_at = Some(now);
        });
        s.id
    }

    fn stored(ctx: &AppState) -> Option<String> {
        let c = ctx.sqlite.conn().unwrap();
        crate::db::sqlite::auth::latest_active(&c, &ctx.security)
            .unwrap()
            .map(|s| s.broker_id)
    }

    /// SES-01: a Delta (crypto) broker session crosses 03:00 IST untouched:
    /// still connected, its streaming and stored row kept, and after a
    /// restart it resumes once the broker accepts the key; an explicit
    /// logout still ends it. The app's own sessions expire on schedule
    /// all the same. The same steps end a Zerodha broker session.
    #[tokio::test]
    async fn a_crypto_session_has_no_daily_boundary() {
        let (t, delta) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        let s = sign_in(ctx, "deltaexchange");
        ctx.runtime.activate(ctx, &s).await;
        let web = browser(ctx);
        for (last, now, fires) in [
            (ist(5, 2, 0, 0), ist(5, 2, 59, 0), false),
            (ist(5, 2, 59, 0), ist(5, 3, 0, 0), true),
            (ist(5, 3, 0, 0), ist(5, 3, 1, 0), false),
        ] {
            t.clock.set(now);
            assert_eq!(expire_if_crossed(ctx, last).await, fires, "{}", now);
        }
        assert!(ctx.is_broker_connected());
        assert_eq!(
            ctx.runtime.active_broker().as_deref(),
            Some("deltaexchange")
        );
        assert_eq!(stored(ctx).as_deref(), Some("deltaexchange"));
        // The app session from before the boundary expired on schedule.
        assert!(ctx.sessions.get(&web, ctx.now()).is_none());
        // A restart after the boundary resumes it, but only once the broker
        // accepts the stored key.
        ctx.runtime.teardown(ctx).await;
        ctx.set_broker_session(None);
        *delta.funds_ok.lock() = false;
        assert!(BrokerAuthService::try_resume(ctx).await.unwrap().is_none());
        *delta.funds_ok.lock() = true;
        let resumed = BrokerAuthService::try_resume(ctx).await.unwrap();
        assert_eq!(
            resumed.map(|s| s.broker_id).as_deref(),
            Some("deltaexchange")
        );
        // An explicit logout still ends it.
        BrokerAuthService::revoke(ctx, crate::events::SessionEndReason::Logout)
            .await
            .unwrap();
        assert!(!ctx.is_broker_connected());
        assert_eq!(stored(ctx), None);

        let (t, _) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        let s = sign_in(ctx, "zerodha");
        ctx.runtime.activate(ctx, &s).await;
        let web = browser(ctx);
        t.clock.set(ist(5, 3, 1, 0));
        assert!(expire_if_crossed(ctx, ist(5, 2, 59, 0)).await);
        assert!(!ctx.is_broker_connected());
        assert_eq!(ctx.runtime.active_broker(), None);
        assert!(ctx.sessions.get(&web, ctx.now()).is_none());
        assert_eq!(stored(ctx), None);
        ctx.runtime.teardown(ctx).await;
    }

    /// SES-02: the poll that notices the boundary late (last check 02:59:50,
    /// a sign-in at 03:00:05, the poll at 03:00:20) ends only what was
    /// authenticated before 03:00: the new broker session, its streaming,
    /// its stored row and its browser session stay; a browser session from
    /// 02:00 ends.
    #[tokio::test]
    async fn a_sign_in_after_the_boundary_survives_a_late_poll() {
        let (t, _) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        let old_web = browser(ctx);
        let old = sign_in(ctx, "zerodha");
        ctx.runtime.activate(ctx, &old).await;
        t.clock.set(ist(5, 3, 0, 5));
        let fresh = sign_in(ctx, "zerodha");
        ctx.runtime.activate(ctx, &fresh).await;
        let web = browser(ctx);
        t.clock.set(ist(5, 3, 0, 20));
        assert!(expire_if_crossed(ctx, ist(5, 2, 59, 50)).await);
        assert!(ctx.is_broker_connected());
        assert_eq!(
            ctx.get_broker_session().map(|s| s.authenticated_at),
            Some(fresh.authenticated_at)
        );
        assert_eq!(ctx.runtime.active_broker().as_deref(), Some("zerodha"));
        assert_eq!(stored(ctx).as_deref(), Some("zerodha"));
        assert!(ctx.sessions.get(&web, ctx.now()).is_some());
        assert!(ctx.sessions.get(&old_web, ctx.now()).is_none());
        ctx.runtime.teardown(ctx).await;
    }

    /// SES-02 after sleep: the clock jumps from 02:00 to 10:00 the next day
    /// (a laptop woke up). The first poll ends the old session once; a
    /// sign-in after the wake survives the next poll.
    #[tokio::test]
    async fn a_wake_from_sleep_ends_the_old_session_once() {
        let (t, _) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        let s = sign_in(ctx, "zerodha");
        ctx.runtime.activate(ctx, &s).await;
        let last = ctx.now();
        t.clock.set(ist(6, 10, 0, 0));
        // The sign-in made right after the wake, before the first poll.
        let fresh = sign_in(ctx, "zerodha");
        ctx.runtime.activate(ctx, &fresh).await;
        assert!(expire_if_crossed(ctx, last).await);
        assert!(ctx.is_broker_connected());
        assert_eq!(stored(ctx).as_deref(), Some("zerodha"));
        assert!(!expire_if_crossed(ctx, ctx.now()).await);
        ctx.runtime.teardown(ctx).await;
    }

    /// A system clock that jumps backward cannot revive an expired session:
    /// once 03:30 has been seen, setting the clock back to 02:00 keeps the
    /// 03:00 boundary, so the session from the day before stays expired in
    /// memory and on resume, and the poll does not fire again for it; a
    /// sign-in made while the clock is behind is stamped with the latest
    /// time seen and stays usable.
    #[tokio::test]
    async fn a_backward_clock_jump_does_not_revive_an_expired_session() {
        let (t, _) = harness(ist(5, 10, 0, 0));
        let ctx = &t.ctx;
        let old = sign_in(ctx, "zerodha");
        t.clock.set(ist(6, 3, 30, 0));
        assert!(!ctx.is_broker_connected(), "expired after 03:00");
        // The clock goes back to before the boundary.
        t.clock.set(ist(6, 2, 0, 0));
        ctx.set_broker_session(Some(old.clone()));
        assert!(!ctx.is_broker_connected(), "revived by the clock");
        assert!(BrokerAuthService::try_resume(ctx).await.unwrap().is_none());
        assert_eq!(stored(ctx), None);
        assert!(!expire_if_crossed(ctx, ist(6, 3, 30, 0)).await);
        // A sign-in while the clock is behind.
        let fresh = sign_in(ctx, "zerodha");
        assert!(fresh.authenticated_at >= ist(6, 3, 30, 0));
        assert!(ctx.is_broker_connected());
    }

    /// 14-N2: a continuous (crypto) session refreshes its master once when
    /// the UTC day turns, including across a clock jump; not again the same
    /// day, and never for an Indian broker.
    #[tokio::test]
    async fn a_crypto_master_is_refreshed_when_the_utc_day_turns() {
        let utc = |d: u32, h: u32| Utc.with_ymd_and_hms(2026, 10, d, h, 0, 0).unwrap();
        let (t, delta) = harness(utc(5, 22));
        let ctx = &t.ctx;
        sign_in(ctx, "deltaexchange");
        let broker: Arc<dyn Broker> = delta.clone();
        master_contract_service::download(ctx, &broker, &AuthToken::new("key:secret"))
            .await
            .unwrap();
        let downloads = || {
            delta
                .calls
                .lock()
                .iter()
                .filter(|c| **c == MockCall::MasterContract)
                .count()
        };
        assert_eq!(downloads(), 1);
        // Same UTC day: nothing.
        t.clock.set(utc(5, 23));
        assert!(!refresh_master_if_day_turned(ctx, utc(5, 22)));
        // The day turns (after a jump of several hours).
        t.clock.set(utc(6, 4));
        assert!(refresh_master_if_day_turned(ctx, utc(5, 23)));
        for _ in 0..500 {
            if downloads() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(downloads(), 2);
        assert!(!refresh_master_if_day_turned(ctx, utc(6, 4)));
        ctx.runtime.teardown(ctx).await;

        let (t, _) = harness(utc(5, 22));
        let ctx = &t.ctx;
        sign_in(ctx, "zerodha");
        t.clock.set(utc(6, 4));
        assert!(!refresh_master_if_day_turned(ctx, utc(5, 22)));
    }
}
