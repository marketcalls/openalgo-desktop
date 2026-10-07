//! Telegram and WhatsApp, native (no Python): the web's bots, alert
//! services and their subscribers.
//!
//! `Messaging` lives on the app context. Its two services own their tasks
//! (Telegram polling, WhatsApp pairing and client) and end them on stop and
//! at app exit (`shutdown`). Order alerts are two bus subscribers registered
//! by `register`.

pub mod alerts;
pub mod format;
pub mod openalgo;
pub mod sealed;
pub mod telegram;
pub mod whatsapp;

use crate::events::subscribers::UiEmitter;
use crate::events::Lane;
use crate::state::AppState;
use parking_lot::RwLock;
use serde_json::Value;
use std::sync::Arc;

/// Who a reply goes to, for the reply caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyClass {
    /// Not linked (or not the owner): one reply per sender per hour.
    Stranger,
    /// A linked user or the owner: `MEMBER_PER_MINUTE` replies a minute.
    Member,
}

#[derive(Default)]
struct ReplyEntry {
    times: std::collections::VecDeque<chrono::DateTime<chrono::Utc>>,
    last: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Default)]
struct ReplyState {
    senders: std::collections::HashMap<String, ReplyEntry>,
    global: std::collections::VecDeque<chrono::DateTime<chrono::Utc>>,
    /// Start of the window in which a drop was last logged.
    logged: Option<chrono::DateTime<chrono::Utc>>,
}

/// Caps on what a bot sends back, so strangers, groups or a looping peer
/// cannot make it send without bound: one reply per hour to a stranger,
/// `MEMBER_PER_MINUTE` a minute to a member, `GLOBAL_PER_MINUTE` a minute in
/// all. Excess is dropped and logged once a minute. The sender map holds at
/// most `MAX_SENDERS` entries: stale ones expire, then the least recently
/// seen go.
#[derive(Default)]
pub struct ReplyLimiter {
    state: parking_lot::Mutex<ReplyState>,
}

impl ReplyLimiter {
    pub const MEMBER_PER_MINUTE: usize = 20;
    pub const GLOBAL_PER_MINUTE: usize = 60;
    pub const MAX_SENDERS: usize = 10_000;

    pub fn sender_count(&self) -> usize {
        self.state.lock().senders.len()
    }

    /// Whether a reply to `who` may go out now; counts it when it may.
    pub fn allow(&self, who: &str, class: ReplyClass, now: chrono::DateTime<chrono::Utc>) -> bool {
        use chrono::Duration as D;
        let mut s = self.state.lock();
        let minute = D::minutes(1);
        let (limit, window) = match class {
            ReplyClass::Stranger => (1usize, D::hours(1)),
            ReplyClass::Member => (Self::MEMBER_PER_MINUTE, minute),
        };
        if !s.senders.contains_key(who) && s.senders.len() >= Self::MAX_SENDERS {
            s.senders
                .retain(|_, e| e.last.map(|t| now - t < D::hours(1)).unwrap_or(false));
            if s.senders.len() >= Self::MAX_SENDERS {
                // Drop the least recently seen tenth in one pass.
                let mut by_age: Vec<_> =
                    s.senders.iter().map(|(k, e)| (e.last, k.clone())).collect();
                by_age.sort_unstable();
                for (_, k) in by_age.into_iter().take(Self::MAX_SENDERS / 10) {
                    s.senders.remove(&k);
                }
            }
        }
        while s
            .global
            .front()
            .map(|t| now - *t >= minute)
            .unwrap_or(false)
        {
            s.global.pop_front();
        }
        let e = s.senders.entry(who.to_string()).or_default();
        e.last = Some(now);
        while e.times.front().map(|t| now - *t >= window).unwrap_or(false) {
            e.times.pop_front();
        }
        let sender_ok = e.times.len() < limit;
        let ok = sender_ok && s.global.len() < Self::GLOBAL_PER_MINUTE;
        if ok {
            if let Some(e) = s.senders.get_mut(who) {
                e.times.push_back(now);
            }
            s.global.push_back(now);
        } else if s.logged.map(|t| now - t >= minute).unwrap_or(true) {
            s.logged = Some(now);
            tracing::warn!("Bot replies over the limit were dropped");
        }
        ok
    }
}

/// What a sender over the link-attempt limit is told (no hint about keys).
pub const TOO_MANY_LINK_ATTEMPTS: &str =
    "Too many link attempts. Wait a few minutes before trying again.";

/// Failed API-key checks per chat sender (`tg:<id>`), the bot form of the
/// `/api/v1` failed-key throttle: 10 per minute, and an hour's lockout once
/// a sender reaches 30 failures within an hour. Bounded: old senders are
/// swept, and the map never holds more than `MAX_SENDERS`.
#[derive(Default)]
pub struct SenderThrottle {
    map: parking_lot::Mutex<
        std::collections::HashMap<
            String,
            std::collections::VecDeque<chrono::DateTime<chrono::Utc>>,
        >,
    >,
}

impl SenderThrottle {
    pub const PER_MINUTE: usize = 10;
    pub const PER_HOUR: usize = 30;
    const MAX_SENDERS: usize = 10_000;

    /// Whether a key from `who` may be checked now.
    pub fn allowed(&self, who: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
        let map = self.map.lock();
        let Some(q) = map.get(who) else { return true };
        let minute = q
            .iter()
            .filter(|t| now - **t < chrono::Duration::minutes(1))
            .count();
        let hour = q
            .iter()
            .filter(|t| now - **t < chrono::Duration::hours(1))
            .count();
        minute < Self::PER_MINUTE && hour < Self::PER_HOUR
    }

    pub fn fail(&self, who: &str, now: chrono::DateTime<chrono::Utc>) {
        let mut map = self.map.lock();
        if map.len() >= Self::MAX_SENDERS && !map.contains_key(who) {
            map.retain(|_, q| {
                q.back()
                    .map(|t| now - *t < chrono::Duration::hours(1))
                    .unwrap_or(false)
            });
            if map.len() >= Self::MAX_SENDERS {
                map.clear();
            }
        }
        let q = map.entry(who.to_string()).or_default();
        while q
            .front()
            .map(|t| now - *t >= chrono::Duration::hours(1))
            .unwrap_or(false)
        {
            q.pop_front();
        }
        q.push_back(now);
        while q.len() > Self::PER_HOUR {
            q.pop_front();
        }
    }

    pub fn clear(&self, who: &str) {
        self.map.lock().remove(who);
    }
}

#[derive(Default)]
pub struct Messaging {
    pub telegram: telegram::TelegramService,
    pub whatsapp: whatsapp::WhatsAppService,
    /// Failed `/link` key checks per sender.
    pub link_throttle: SenderThrottle,
    /// Reply caps of the Telegram bot.
    pub telegram_replies: ReplyLimiter,
    /// Reply caps of the WhatsApp bot (it answers from the trader's number).
    pub whatsapp_replies: ReplyLimiter,
    /// Tests: record Socket.IO events instead of sending them.
    emitter: RwLock<Option<Arc<dyn UiEmitter>>>,
}

impl Messaging {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_emitter(&self, e: Option<Arc<dyn UiEmitter>>) {
        *self.emitter.write() = e;
    }

    /// End every messaging task (app exit).
    pub async fn shutdown(&self) {
        self.telegram.shutdown().await;
        self.whatsapp.shutdown().await;
    }
}

/// Push a Socket.IO event to the pages.
pub async fn emit(ctx: &AppState, event: &str, payload: Value) {
    let over = ctx.messaging.emitter.read().clone();
    match over {
        Some(e) => e.emit(event, payload).await,
        None => ctx.ui.emit(event, payload).await,
    }
}

/// The desktop's single OpenAlgo account (web `get_username_by_apikey` of
/// the key on an order: there is only one).
pub fn account_username(ctx: &AppState) -> Option<String> {
    if let Some(u) = ctx.signed_in_user() {
        return Some(u);
    }
    ctx.sqlite
        .conn()
        .and_then(|c| crate::db::sqlite::user::find_first(&c))
        .ok()
        .flatten()
        .map(|u| u.username)
}

/// Register the alert subscribers. Call once, inside the runtime.
pub fn register(ctx: &Arc<AppState>) {
    let weak = Arc::downgrade(ctx);
    ctx.bus.subscribe(
        Arc::new(alerts::TelegramAlerts::new(weak.clone())),
        Lane::BestEffort,
    );
    ctx.bus.subscribe(
        Arc::new(alerts::WhatsAppAlerts::new(weak)),
        Lane::BestEffort,
    );
}

/// Bring the bots back after a launch, as the web's start-up does: Telegram
/// when it was started, WhatsApp when a device is paired.
pub fn autostart(ctx: &Arc<AppState>) {
    let weak = Arc::downgrade(ctx);
    ctx.spawn(async move {
        let Some(ctx) = weak.upgrade() else { return };
        if telegram::TelegramService::is_bot_active(&ctx) {
            let token = ctx
                .sqlite
                .conn()
                .and_then(|c| telegram::db::get_bot_config(&c, &ctx.security))
                .ok()
                .and_then(|c| c.token);
            if let Some(token) = token {
                let (ok, msg) = ctx.messaging.telegram.initialize(&ctx, &token).await;
                if ok {
                    let (ok, msg) = ctx.messaging.telegram.start(&ctx).await;
                    tracing::info!("Telegram bot auto-start: {} ({})", ok, msg);
                } else {
                    tracing::error!("Telegram bot auto-start refused: {}", msg);
                }
            }
        }
        if whatsapp::WhatsAppService::is_paired(&ctx) {
            let (ok, msg) = ctx.messaging.whatsapp.start_bot(&ctx).await;
            tracing::info!("WhatsApp bot auto-start: {} ({})", ok, msg);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 10, 7, 10, 0, 0).unwrap()
    }

    #[test]
    fn a_stranger_gets_one_reply_an_hour() {
        let r = ReplyLimiter::default();
        let sent = (0..500)
            .filter(|i| {
                r.allow(
                    "wa:x",
                    ReplyClass::Stranger,
                    t0() + chrono::Duration::seconds(*i),
                )
            })
            .count();
        assert_eq!(sent, 1);
        assert!(r.allow(
            "wa:x",
            ReplyClass::Stranger,
            t0() + chrono::Duration::minutes(61)
        ));
    }

    #[test]
    fn members_and_the_bot_are_capped_per_minute() {
        let r = ReplyLimiter::default();
        let n = (0..100)
            .filter(|_| r.allow("tg:1", ReplyClass::Member, t0()))
            .count();
        assert_eq!(n, ReplyLimiter::MEMBER_PER_MINUTE);
        assert!(!r.allow(
            "tg:1",
            ReplyClass::Member,
            t0() + chrono::Duration::seconds(59)
        ));
        assert!(r.allow(
            "tg:1",
            ReplyClass::Member,
            t0() + chrono::Duration::seconds(61)
        ));
        // Global cap across many members.
        let r = ReplyLimiter::default();
        let n = (0..200)
            .filter(|i| r.allow(&format!("tg:{}", i), ReplyClass::Member, t0()))
            .count();
        assert_eq!(n, ReplyLimiter::GLOBAL_PER_MINUTE);
    }

    #[test]
    fn the_sender_map_stays_bounded() {
        let r = ReplyLimiter::default();
        for i in 0..25_000 {
            r.allow(
                &format!("wa:{}", i),
                ReplyClass::Stranger,
                t0() + chrono::Duration::milliseconds(i),
            );
        }
        assert!(r.sender_count() <= ReplyLimiter::MAX_SENDERS);
    }

    #[test]
    fn link_throttle_minute_and_hour() {
        let t = SenderThrottle::default();
        let t0 = chrono::Utc.with_ymd_and_hms(2026, 10, 7, 10, 0, 0).unwrap();
        for _ in 0..10 {
            assert!(t.allowed("tg:1", t0));
            t.fail("tg:1", t0);
        }
        assert!(!t.allowed("tg:1", t0));
        assert!(t.allowed("tg:2", t0));
        // The minute passes: allowed again, until 30 within the hour.
        let t1 = t0 + chrono::Duration::seconds(61);
        assert!(t.allowed("tg:1", t1));
        for _ in 0..10 {
            t.fail("tg:1", t1);
        }
        let t2 = t1 + chrono::Duration::seconds(61);
        for _ in 0..10 {
            t.fail("tg:1", t2);
        }
        let t3 = t2 + chrono::Duration::seconds(61);
        assert!(!t.allowed("tg:1", t3), "locked for the hour");
        assert!(t.allowed("tg:1", t0 + chrono::Duration::minutes(63)));
        t.clear("tg:1");
        assert!(t.allowed("tg:1", t3));
    }
}
