//! Sessions: the daily boundary, browser sessions, and the task that ends
//! what came before the boundary.

pub mod boundary;
pub mod web;

use crate::brokers::catalog;
use crate::brokers::types::AuthToken;
use crate::services::broker_auth_service::BrokerAuthService;
use crate::services::master_contract_service;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Duration;
use web::WebSession;

/// Poll interval of the expiry task. Short enough that a machine waking from
/// sleep past 03:00 expires the session within a minute, and long enough to
/// cost nothing.
pub const EXPIRY_POLL: Duration = Duration::from_secs(30);

/// One step of the expiry task: if a boundary passed after `last_check`,
/// end what was authenticated before it: the broker session, its
/// streaming and stored row (`BrokerAuthService::expire_before`), and the
/// app's browser sessions. A sign-in made after the boundary, before this
/// poll or right after a wake from sleep, stays (SES-02). The app's own
/// sessions expire at the boundary for every broker; only the broker
/// session of a continuous (crypto) broker is exempt (SES-01). Returns
/// whether the boundary fired.
pub async fn expire_if_crossed(ctx: &AppState, last_check: DateTime<Utc>) -> bool {
    let now = ctx.now();
    let cfg = ctx.server_config();
    let boundary = boundary::last_boundary(now, cfg.session_expiry_hour, cfg.session_expiry_minute);
    if !(last_check < boundary && now >= boundary) {
        return false;
    }
    tracing::info!("Daily session boundary reached; ending sessions from before it");
    if let Err(e) = BrokerAuthService::expire_before(ctx, boundary).await {
        tracing::error!(
            "Broker session ended at the boundary; its stored row is still to be revoked: {}",
            e
        );
    }
    ctx.sessions.end_before(boundary);
    true
}

/// The browser session `id` as a request sees it now. A sign-in from
/// before the most recent daily boundary is not a current login (web
/// e2bdd061e, `is_session_valid`): the session comes back signed out, so
/// every route, the live-update socket and its pushes treat the cookie like
/// an anonymous one, also in the seconds before the expiry poll ends it
/// ([`EXPIRY_POLL`], and after a wake from sleep). Nothing is ended or
/// revoked here: open tabs poll, and a stale one must not end what another
/// device is using; [`expire_if_crossed`] does that.
pub fn web_session(ctx: &AppState, id: &str) -> Option<WebSession> {
    let now = ctx.now();
    let (hour, minute) = {
        let cfg = ctx.config.read();
        (cfg.session_expiry_hour, cfg.session_expiry_minute)
    };
    let mut s = ctx.sessions.get(id, now)?;
    if s.user.is_some() && !s.since(boundary::last_boundary(now, hour, minute)) {
        s.user = None;
    }
    Some(s)
}

/// A continuous (crypto) session is not renewed at the daily boundary, so
/// its master contract is refreshed when the UTC day turns between
/// `last_check` and now (Delta lists new expiries every day; its master
/// follows the UTC day, so the smart rule then downloads once). The refresh
/// runs as a task of the session (aborted with it) and never interrupts it:
/// the stored master stays loaded until the new one replaces it in one
/// swap, feed subscriptions are only reconciled, and a failed download
/// leaves the session and its master as they were. Returns whether a
/// refresh started (14-N2).
pub fn refresh_master_if_day_turned(ctx: &AppState, last_check: DateTime<Utc>) -> bool {
    let now = ctx.now();
    if last_check.date_naive() == now.date_naive() {
        return false;
    }
    let Some(s) = ctx.get_broker_session() else {
        return false;
    };
    if catalog::has_daily_boundary(&s.broker_id) {
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
            Err(e) => tracing::warn!(
                "Master contract refresh failed; the loaded one stays in use: {}",
                e
            ),
        }
    });
    true
}

/// One tick of the session poll: finish a broker revoke whose database
/// write failed earlier (`BrokerAuthService::retry_pending_revoke`), forget
/// sign-ins left waiting for their authenticator code, then enforce the
/// daily boundary, then refresh a crypto session's master when the UTC day
/// turned. Returns whether the boundary fired.
pub async fn poll_tick(ctx: &AppState, last_check: DateTime<Utc>) -> bool {
    if let Err(e) = BrokerAuthService::retry_pending_revoke(ctx) {
        tracing::warn!("The ended broker session is still to be revoked: {}", e);
    }
    // Keys a sign-in opened and then left waiting for its code go (KEY-01).
    ctx.sessions.drop_stale_sign_ins(ctx.now());
    let fired = expire_if_crossed(ctx, last_check).await;
    refresh_master_if_day_turned(ctx, last_check);
    fired
}

/// Start the owned background task that enforces the daily boundary and
/// retries a pending broker revoke.
pub fn spawn_expiry_task(ctx: Arc<AppState>) {
    let token = ctx.shutdown.clone();
    // Weak: the context owns this task, so a strong reference would be a cycle.
    let weak = Arc::downgrade(&ctx);
    let mut last = ctx.now();
    ctx.spawn(async move {
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(EXPIRY_POLL) => {
                    let Some(c) = weak.upgrade() else { break };
                    poll_tick(&c, last).await;
                    last = c.now();
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::common::symbols::tests::row;
    use crate::brokers::mock::{MockBroker, MockCall, PlaceGate};
    use crate::brokers::{Broker, BrokerRegistry};
    use crate::events::{bus::Lane, bus::Subscriber, Event, SessionEndReason, Topic};
    use crate::security::Secret;
    use crate::services::broker_auth_service::FormLogin;
    use crate::state::BrokerSession;
    use chrono::TimeZone;
    use chrono_tz::Asia::Kolkata;
    use parking_lot::Mutex;

    fn ist(d: u32, h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Kolkata
            .with_ymd_and_hms(2026, 10, d, h, m, s)
            .unwrap()
            .with_timezone(&Utc)
    }

    struct Brokers {
        delta: Arc<MockBroker>,
        angel: Arc<MockBroker>,
    }

    fn harness(now: DateTime<Utc>) -> (crate::state::testing::TestCtx, Brokers) {
        let delta = Arc::new(MockBroker::new("deltaexchange"));
        let zerodha = Arc::new(MockBroker::new("zerodha"));
        let angel = Arc::new(MockBroker::new("angel"));
        *delta.master.lock() = Some(Ok(vec![row("BTCUSDFUT", "BTCUSD", "CRYPTO", "27")]));
        *zerodha.master.lock() = Some(Ok(vec![row("SBIN", "SBIN", "NSE", "779521")]));
        *angel.master.lock() = Some(Ok(vec![row("SBIN", "SBIN-EQ", "NSE", "3045")]));
        *angel.auth_user_id.lock() = "U1".into();
        let t = crate::state::testing::build(
            BrokerRegistry::with(vec![
                delta.clone() as Arc<dyn Broker>,
                zerodha as Arc<dyn Broker>,
                angel.clone() as Arc<dyn Broker>,
            ]),
            now,
        );
        (t, Brokers { delta, angel })
    }

    /// Store a session signed in now, as a sign-in does (no streaming).
    fn sign_in(ctx: &AppState, broker: &str) -> BrokerSession {
        let s = BrokerSession {
            broker_id: broker.into(),
            auth_token: Secret::new("mock-access-token"),
            feed_token: None,
            user_id: "U1".into(),
            user_name: None,
            authenticated_at: ctx.now(),
        };
        BrokerAuthService::persist(ctx, &s).unwrap();
        s
    }

    /// A signed-in browser session, signed in now.
    fn browser(ctx: &AppState) -> String {
        let s = ctx.sessions.create(ctx.now());
        let now = ctx.now();
        ctx.sessions.update(&s.id, |x| {
            x.user = Some("trader".into());
            x.authenticated_at = Some(now);
        });
        s.id
    }

    fn stored(ctx: &AppState) -> Option<(String, DateTime<Utc>)> {
        let c = ctx.sqlite.conn().unwrap();
        crate::db::sqlite::auth::latest_active(&c, &ctx.security)
            .unwrap()
            .map(|s| (s.broker_id, s.authenticated_at))
    }

    /// Records the broker session ends published on the bus.
    #[derive(Default)]
    struct Ends(Mutex<Vec<SessionEndReason>>);

    #[async_trait::async_trait]
    impl Subscriber for Ends {
        fn name(&self) -> &'static str {
            "session_ends"
        }
        fn topics(&self) -> Vec<Topic> {
            vec![Topic::BrokerSessionEnded]
        }
        async fn handle(&self, event: Arc<Event>) {
            if let Event::BrokerSessionEnded { reason } = &*event {
                self.0.lock().push(*reason);
            }
        }
    }

    fn ends(ctx: &AppState) -> Arc<Ends> {
        let e = Arc::new(Ends::default());
        ctx.bus.subscribe(e.clone(), Lane::Critical);
        e
    }

    async fn until(what: &str, cond: impl Fn() -> bool) {
        for _ in 0..500 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {}", what);
    }

    /// The session ends `e` has received so far, once every event published
    /// before this call has reached it (a marker is published and awaited;
    /// one subscriber's events arrive in order). The marker is left out.
    async fn received(ctx: &AppState, e: &Ends) -> Vec<SessionEndReason> {
        let marker = SessionEndReason::PasswordChanged;
        let before = e.0.lock().iter().filter(|r| **r == marker).count();
        ctx.bus
            .publish(Event::BrokerSessionEnded { reason: marker });
        until("the bus to deliver", || {
            e.0.lock().iter().filter(|r| **r == marker).count() > before
        })
        .await;
        let v = e.0.lock().clone();
        v.into_iter().filter(|r| *r != marker).collect()
    }

    /// SES-01: a Delta (crypto) broker session crosses 03:00 IST untouched:
    /// still connected, its streaming and stored row kept, nothing torn down
    /// and no session end announced; after a restart past the boundary it
    /// resumes, but only once the broker accepts the key; an explicit logout
    /// still ends it. The app's own browser sessions expire on schedule all
    /// the same. The same steps end a Zerodha broker session.
    #[tokio::test]
    async fn a_crypto_session_has_no_daily_boundary() {
        let (t, b) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        let seen = ends(ctx);
        let s = sign_in(ctx, "deltaexchange");
        ctx.runtime.activate(ctx, &s).await;
        let web = browser(ctx);
        for (last, now, fires) in [
            (ist(5, 2, 0, 0), ist(5, 2, 59, 0), false),
            (ist(5, 2, 59, 0), ist(5, 3, 0, 0), true),
            (ist(5, 3, 0, 0), ist(5, 3, 1, 0), false),
        ] {
            t.clock.set(now);
            assert_eq!(poll_tick(ctx, last).await, fires, "{}", now);
        }
        assert!(ctx.is_broker_connected());
        assert_eq!(
            ctx.runtime.active_broker().as_deref(),
            Some("deltaexchange")
        );
        assert_eq!(*b.delta.logouts.lock(), 0, "streaming was torn down");
        assert_eq!(stored(ctx).map(|s| s.0).as_deref(), Some("deltaexchange"));
        assert_eq!(received(ctx, &seen).await, vec![]);
        // The app session from before the boundary expired on schedule.
        assert!(ctx.sessions.get(&web, ctx.now()).is_none());
        // A day later the session is still live.
        t.clock.set(ist(6, 3, 1, 0));
        assert!(poll_tick(ctx, ist(6, 2, 59, 0)).await);
        assert!(ctx.is_broker_connected());
        // A restart after the boundary resumes it, but only once the broker
        // accepts the stored key.
        ctx.runtime.teardown(ctx).await;
        ctx.set_broker_session(None);
        *b.delta.funds_ok.lock() = false;
        assert!(BrokerAuthService::try_resume(ctx).await.unwrap().is_none());
        *b.delta.funds_ok.lock() = true;
        let resumed = BrokerAuthService::try_resume(ctx).await.unwrap();
        assert_eq!(
            resumed.map(|s| s.broker_id).as_deref(),
            Some("deltaexchange")
        );
        // An explicit logout still ends it.
        BrokerAuthService::revoke(ctx, SessionEndReason::Logout)
            .await
            .unwrap();
        assert!(!ctx.is_broker_connected());
        assert_eq!(stored(ctx), None);

        let (t, _) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        let seen = ends(ctx);
        let s = sign_in(ctx, "zerodha");
        ctx.runtime.activate(ctx, &s).await;
        let web = browser(ctx);
        t.clock.set(ist(5, 3, 1, 0));
        assert!(poll_tick(ctx, ist(5, 2, 59, 0)).await);
        assert!(!ctx.is_broker_connected());
        assert_eq!(ctx.runtime.active_broker(), None);
        assert!(ctx.sessions.get(&web, ctx.now()).is_none());
        assert_eq!(stored(ctx), None);
        assert_eq!(
            received(ctx, &seen).await,
            vec![SessionEndReason::DailyExpiry]
        );
    }

    /// SES-02: the poll that notices the boundary late (last check 02:59:50,
    /// a sign-in at 03:00:05, the poll at 03:00:20) ends only what was
    /// authenticated before 03:00: the new broker session, its streaming,
    /// its stored row and its browser session stay, and no session end is
    /// announced; a browser session from 02:00 ends.
    #[tokio::test]
    async fn a_sign_in_after_the_boundary_survives_a_late_poll() {
        let (t, _) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        let seen = ends(ctx);
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
        assert_eq!(
            stored(ctx),
            Some(("zerodha".to_string(), fresh.authenticated_at))
        );
        assert!(ctx.sessions.get(&web, ctx.now()).is_some());
        assert!(ctx.sessions.get(&old_web, ctx.now()).is_none());
        assert_eq!(received(ctx, &seen).await, vec![]);
        ctx.runtime.teardown(ctx).await;
    }

    /// SES-02 when the poll is late because the computer slept: the clock
    /// jumps from 02:00 to 10:00 the next day. A sign-in made right after
    /// the wake, before the first poll, survives it; the poll fires once.
    #[tokio::test]
    async fn a_wake_from_sleep_keeps_the_sign_in_made_after_it() {
        let (t, _) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        let s = sign_in(ctx, "zerodha");
        ctx.runtime.activate(ctx, &s).await;
        let last = ctx.now();
        t.clock.set(ist(6, 10, 0, 0));
        let fresh = sign_in(ctx, "zerodha");
        ctx.runtime.activate(ctx, &fresh).await;
        assert!(expire_if_crossed(ctx, last).await);
        assert!(ctx.is_broker_connected());
        assert_eq!(ctx.runtime.active_broker().as_deref(), Some("zerodha"));
        assert_eq!(
            stored(ctx),
            Some(("zerodha".to_string(), fresh.authenticated_at))
        );
        assert!(!expire_if_crossed(ctx, ctx.now()).await);
        ctx.runtime.teardown(ctx).await;
    }

    /// SES-02, the race itself: a real sign-in that completes while the late
    /// poll runs (both after 03:00, the old session from 02:00 still live).
    /// Whichever finishes first, the new session, its streaming and its
    /// stored row stay; only the old one ends.
    #[tokio::test]
    async fn a_sign_in_racing_the_boundary_poll_is_kept() {
        let (t, b) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        {
            let conn = ctx.sqlite.conn().unwrap();
            crate::db::sqlite::credentials::save(
                &conn,
                &ctx.security,
                "angel",
                crate::db::sqlite::credentials::CredentialUpdate {
                    api_key: Some("angelkey".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let old = sign_in(ctx, "angel");
        ctx.runtime.activate(ctx, &old).await;
        t.clock.set(ist(5, 3, 0, 5));
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        *b.angel.auth_gate.lock() = Some(gate.clone());
        let form = FormLogin {
            client_id: Some("U1".into()),
            password: Some(Secret::new("pin")),
            totp: Some(Secret::new("123456")),
            dob: None,
        };
        let login = BrokerAuthService::login_with_form(ctx, "angel", form);
        let poll = async {
            // Once the broker has answered the sign-in.
            gate.wait().await;
            expire_if_crossed(ctx, ist(5, 2, 59, 50)).await
        };
        let (fresh, fired) = tokio::join!(login, poll);
        let fresh = fresh.expect("signed in");
        assert!(fired);
        assert!(fresh.authenticated_at > old.authenticated_at);
        assert_eq!(
            ctx.get_broker_session().map(|s| s.authenticated_at),
            Some(fresh.authenticated_at)
        );
        assert_eq!(ctx.runtime.active_broker().as_deref(), Some("angel"));
        assert_eq!(
            stored(ctx),
            Some(("angel".to_string(), fresh.authenticated_at))
        );
        ctx.runtime.teardown(ctx).await;
    }

    /// The database refuses every change to a stored broker session.
    fn fail_auth_writes(ctx: &AppState) {
        ctx.sqlite
            .conn()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER test_fail_auth_update BEFORE UPDATE ON auth
                 BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
            )
            .unwrap();
    }

    fn allow_auth_writes(ctx: &AppState) {
        ctx.sqlite
            .conn()
            .unwrap()
            .execute_batch("DROP TRIGGER test_fail_auth_update")
            .unwrap();
    }

    /// Whether `broker`'s stored session row is revoked.
    fn revoked(ctx: &AppState, broker: &str) -> bool {
        ctx.sqlite
            .conn()
            .unwrap()
            .query_row(
                "SELECT is_revoked FROM auth WHERE broker_id = ?1",
                [broker],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }

    /// SES-02 with SEC-05b: when the boundary's revoke cannot be written
    /// while a session signed in after the boundary is live, the revoke is
    /// still left pending (like a failed sign-out) and finished once the
    /// database recovers, but only for what came before the boundary: the
    /// newer session and its stored row stay. Here Zerodha's 02:00 row is
    /// still stored when Angel signs in at 03:00:05.
    #[tokio::test]
    async fn a_failed_boundary_write_is_finished_without_the_newer_session() {
        let (t, _) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        sign_in(ctx, "zerodha");
        t.clock.set(ist(5, 3, 0, 5));
        let fresh = sign_in(ctx, "angel");
        fail_auth_writes(ctx);
        t.clock.set(ist(5, 3, 0, 20));
        assert!(expire_if_crossed(ctx, ist(5, 2, 59, 50)).await);
        assert!(
            BrokerAuthService::revoke_is_pending(ctx),
            "the boundary's revoke is kept to be written"
        );
        assert_eq!(
            ctx.get_broker_session().map(|s| s.broker_id).as_deref(),
            Some("angel")
        );
        allow_auth_writes(ctx);
        poll_tick(ctx, ctx.now()).await;
        assert!(!BrokerAuthService::revoke_is_pending(ctx));
        assert!(revoked(ctx, "zerodha"), "the 02:00 row is revoked");
        assert!(!revoked(ctx, "angel"));
        assert_eq!(
            stored(ctx),
            Some(("angel".to_string(), fresh.authenticated_at))
        );
        assert_eq!(
            ctx.get_broker_session().map(|s| s.broker_id).as_deref(),
            Some("angel")
        );
    }

    /// SES-01 with SEC-05b: the boundary's revoke that cannot be written
    /// while no session is live (after a restart, before the trader signs
    /// in) is left pending and blocks resume until written, on disk across
    /// a restart; written, it revokes the stale Indian broker row but keeps
    /// the stored crypto session, which then resumes.
    #[tokio::test]
    async fn a_failed_boundary_write_keeps_a_stored_crypto_session() {
        let (t, _) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        sign_in(ctx, "zerodha");
        t.clock.set(ist(5, 2, 30, 0));
        let delta = sign_in(ctx, "deltaexchange");
        // A restart: nothing live in memory.
        ctx.set_broker_session(None);
        fail_auth_writes(ctx);
        t.clock.set(ist(5, 3, 0, 20));
        assert!(expire_if_crossed(ctx, ist(5, 2, 59, 50)).await);
        assert!(BrokerAuthService::revoke_is_pending(ctx));
        // Another restart: only the marker on disk remembers it.
        *ctx.revoke_pending.lock() = None;
        assert!(BrokerAuthService::revoke_is_pending(ctx));
        assert!(
            BrokerAuthService::try_resume(ctx).await.unwrap().is_none(),
            "nothing resumes while the revoke is unwritten"
        );
        allow_auth_writes(ctx);
        let resumed = BrokerAuthService::try_resume(ctx).await.unwrap();
        assert_eq!(
            resumed.map(|s| (s.broker_id, s.authenticated_at)),
            Some(("deltaexchange".to_string(), delta.authenticated_at))
        );
        assert!(!BrokerAuthService::revoke_is_pending(ctx));
        assert!(revoked(ctx, "zerodha"));
        assert!(!revoked(ctx, "deltaexchange"));
        ctx.runtime.teardown(ctx).await;
    }

    /// SEC-05b with the boundary's scoped revoke: a logout whose revoke
    /// cannot be written is never narrowed to the boundary's revoke already
    /// pending on disk, even when the marker cannot be rewritten (a full
    /// disk refuses both). Delta is live with Zerodha's stale row; the
    /// boundary's write fails (pending: before the boundary); the trader
    /// logs out of Delta while the database and the marker write still
    /// fail. After a restart the logged-out Delta session is not resumed.
    #[tokio::test]
    async fn a_failed_logout_is_never_narrowed_to_the_boundarys_revoke() {
        let (t, _) = harness(ist(5, 2, 0, 0));
        let ctx = &t.ctx;
        sign_in(ctx, "zerodha");
        t.clock.set(ist(5, 2, 30, 0));
        sign_in(ctx, "deltaexchange");
        fail_auth_writes(ctx);
        t.clock.set(ist(5, 3, 0, 20));
        assert!(expire_if_crossed(ctx, ist(5, 2, 59, 50)).await);
        assert!(ctx.is_broker_connected(), "Delta has no daily boundary");
        // The marker cannot be rewritten: its temporary file's place is
        // taken.
        std::fs::create_dir(ctx.data_dir.join("broker-revoke.tmp")).unwrap();
        assert!(
            BrokerAuthService::revoke(ctx, SessionEndReason::Logout)
                .await
                .is_err(),
            "the row is still to be revoked"
        );
        // A restart: only the marker on disk remembers the logout.
        *ctx.revoke_pending.lock() = None;
        allow_auth_writes(ctx);
        assert!(
            BrokerAuthService::try_resume(ctx).await.unwrap().is_none(),
            "the logged-out Delta session came back"
        );
        assert!(revoked(ctx, "deltaexchange"));
        assert!(revoked(ctx, "zerodha"));
    }

    /// 14-N2: a continuous (crypto) session refreshes its master once when
    /// the UTC day turns, including across a clock jump; not again the same
    /// day, and never for an Indian broker. The refresh never interrupts
    /// the session: while it downloads, the session, its streaming and the
    /// loaded master stay in use; the new master replaces the old one in one
    /// swap; a refresh that fails leaves the session and the loaded master
    /// as they were.
    #[tokio::test]
    async fn a_crypto_master_refresh_does_not_interrupt_the_session() {
        let utc = |d: u32, h: u32| Utc.with_ymd_and_hms(2026, 10, d, h, 0, 0).unwrap();
        let (t, b) = harness(utc(5, 22));
        let ctx = &t.ctx;
        let seen = ends(ctx);
        let s = sign_in(ctx, "deltaexchange");
        ctx.runtime.activate(ctx, &s).await;
        let downloads = || {
            b.delta
                .calls
                .lock()
                .iter()
                .filter(|c| **c == MockCall::MasterContract)
                .count()
        };
        until("the sign-in master download", || {
            ctx.symbols.by_symbol("CRYPTO", "BTCUSDFUT").is_some()
        })
        .await;
        assert_eq!(downloads(), 1);
        // Same UTC day: nothing.
        t.clock.set(utc(5, 23));
        assert!(!refresh_master_if_day_turned(ctx, utc(5, 22)));

        // The day turns; the new master lists a new contract. Hold the
        // download half way.
        *b.delta.master.lock() = Some(Ok(vec![
            row("BTCUSDFUT", "BTCUSD", "CRYPTO", "27"),
            row("ETHUSDFUT", "ETHUSD", "CRYPTO", "3136"),
        ]));
        let gate = Arc::new(PlaceGate::default());
        *b.delta.master_gate.lock() = Some(gate.clone());
        t.clock.set(utc(6, 4));
        assert!(!poll_tick(ctx, utc(5, 23)).await);
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .expect("the refresh started");
        assert!(ctx.is_broker_connected());
        assert_eq!(
            ctx.runtime.active_broker().as_deref(),
            Some("deltaexchange")
        );
        assert!(ctx.symbols.by_symbol("CRYPTO", "BTCUSDFUT").is_some());
        gate.release.notify_one();
        until("the new master", || {
            ctx.symbols.by_symbol("CRYPTO", "ETHUSDFUT").is_some()
        })
        .await;
        assert_eq!(downloads(), 2);
        assert!(ctx.symbols.by_symbol("CRYPTO", "BTCUSDFUT").is_some());
        assert!(ctx.is_broker_connected());
        assert_eq!(
            ctx.runtime.active_broker().as_deref(),
            Some("deltaexchange")
        );
        assert_eq!(*b.delta.logouts.lock(), 0, "streaming was torn down");
        // Not again the same day.
        *b.delta.master_gate.lock() = None;
        assert!(!refresh_master_if_day_turned(ctx, utc(6, 4)));

        // The next day the download fails: nothing changes.
        *b.delta.master.lock() = Some(Err("Delta is not answering".into()));
        t.clock.set(utc(7, 1));
        assert!(refresh_master_if_day_turned(ctx, utc(6, 23)));
        until("the failed refresh", || downloads() == 3).await;
        until("the failure to be recorded", || {
            !ctx.runtime.claims.is_running("deltaexchange")
        })
        .await;
        assert!(ctx.symbols.by_symbol("CRYPTO", "ETHUSDFUT").is_some());
        assert!(ctx.is_broker_connected());
        assert_eq!(
            ctx.runtime.active_broker().as_deref(),
            Some("deltaexchange")
        );
        assert_eq!(*b.delta.logouts.lock(), 0, "streaming was torn down");
        assert_eq!(received(ctx, &seen).await, vec![]);
        ctx.runtime.teardown(ctx).await;

        let (t, _) = harness(utc(5, 22));
        let ctx = &t.ctx;
        sign_in(ctx, "zerodha");
        t.clock.set(utc(6, 4));
        assert!(!refresh_master_if_day_turned(ctx, utc(5, 22)));
    }
}
