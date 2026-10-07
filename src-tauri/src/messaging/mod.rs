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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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
