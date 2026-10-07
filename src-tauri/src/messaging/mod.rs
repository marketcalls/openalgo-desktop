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

#[derive(Default)]
pub struct Messaging {
    pub telegram: telegram::TelegramService,
    pub whatsapp: whatsapp::WhatsAppService,
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
    ctx.bus
        .subscribe(Arc::new(alerts::WhatsAppAlerts::new(weak)), Lane::BestEffort);
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
