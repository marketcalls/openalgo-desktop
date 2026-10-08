//! Telegram bot (web `services/telegram_bot_service.py` and
//! `services/telegram_alert_service.py`), on the Bot API directly.
//!
//! One owned polling task per start: `deleteWebhook` (dropping the backlog,
//! as the web's `drop_pending_updates=True`), then `getUpdates` long polls,
//! each update handled in turn. Connecting retries with the web's backoff (5,
//! 10, 20, 40 seconds, then gives up); a running poll retries network errors
//! forever with a capped backoff. Every wait is cancellable, so a stop or the
//! app exit ends the task at once; the task holds only a weak reference to
//! the app context.
//!
//! Alerts and admin sends use `sendMessage` with the stored token directly,
//! independent of the polling task, as the web's alert service does. Order
//! alerts are sent only while the bot is started (web issue #1577).

pub mod api;
pub mod bot;
pub mod chart;
pub mod db;

use crate::events::OrderMeta;
use crate::messaging::alerts::{self, Channel};
use crate::security::Secret;
use crate::state::AppState;
use api::{BotApi, TgError, DEFAULT_API_BASE};
use parking_lot::RwLock;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Seconds a start waits for the bot to begin polling before it answers.
pub const BOT_START_WAIT: Duration = Duration::from_secs(5);
/// Seconds a stop waits for the polling task to finish.
pub const BOT_STOP_JOIN: Duration = Duration::from_secs(10);
/// What a start is told while another one is still in progress.
pub const BOT_BUSY_MESSAGE: &str =
    "The bot is still starting or stopping. Check its status in a few seconds.";
/// Connection attempts before a start gives up (web `max_retries`).
const START_MAX_RETRIES: u32 = 5;

/// Timings, adjustable by tests.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// `getUpdates` long-poll seconds.
    pub poll_timeout_secs: u64,
    /// First retry delay while connecting (web `base_delay`, doubled per try).
    pub start_backoff: Duration,
    /// First retry delay of a failing poll, doubled up to `poll_backoff_max`.
    pub poll_backoff: Duration,
    pub poll_backoff_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll_timeout_secs: 25,
            start_backoff: Duration::from_secs(5),
            poll_backoff: Duration::from_secs(1),
            poll_backoff_max: Duration::from_secs(30),
        }
    }
}

struct PollRun {
    gen: u64,
    cancel: CancellationToken,
    join: JoinHandle<()>,
}

pub struct TelegramService {
    api_base: RwLock<String>,
    timing: RwLock<Timing>,
    /// The bot is polling (web `is_running`).
    running: Arc<AtomicBool>,
    gen: AtomicU64,
    /// Start and stop are one at a time.
    lifecycle: tokio::sync::Mutex<Option<PollRun>>,
    /// Polls completed, for tests and diagnostics.
    polls: Arc<AtomicU64>,
}

impl Default for TelegramService {
    fn default() -> Self {
        Self::new()
    }
}

impl TelegramService {
    pub fn new() -> Self {
        Self {
            api_base: RwLock::new(DEFAULT_API_BASE.to_string()),
            timing: RwLock::new(Timing::default()),
            running: Arc::new(AtomicBool::new(false)),
            gen: AtomicU64::new(0),
            lifecycle: tokio::sync::Mutex::new(None),
            polls: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Point the bot at another Bot API server (tests use a local fake).
    pub fn set_api_base(&self, base: &str) {
        *self.api_base.write() = base.to_string();
    }

    pub fn set_timing(&self, t: Timing) {
        *self.timing.write() = t;
    }

    pub fn timing(&self) -> Timing {
        *self.timing.read()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn poll_count(&self) -> u64 {
        self.polls.load(Ordering::SeqCst)
    }

    /// Whether a polling task exists (running, connecting or backing off).
    pub async fn task_alive(&self) -> bool {
        self.lifecycle
            .lock()
            .await
            .as_ref()
            .map(|r| !r.join.is_finished())
            .unwrap_or(false)
    }

    /// Wait until the polling task has ended on its own, up to `within`.
    /// True when no task is left; a finished task is reaped. The task is
    /// not cancelled, so this observes a start that gave up rather than
    /// causing it.
    pub async fn wait_task_end(&self, within: Duration) -> bool {
        let mut lc = self.lifecycle.lock().await;
        let Some(run) = lc.as_mut() else {
            return true;
        };
        if tokio::time::timeout(within, &mut run.join).await.is_err() {
            return false;
        }
        lc.take();
        true
    }

    pub fn api(&self, ctx: &AppState, token: Secret) -> BotApi {
        BotApi::new(ctx.http.clone(), &self.api_base.read(), token)
    }

    fn config(ctx: &AppState) -> crate::error::Result<db::BotConfig> {
        let conn = ctx.sqlite.conn()?;
        db::get_bot_config(&conn, &ctx.security)
    }

    fn update_config(ctx: &AppState, u: db::ConfigUpdate) {
        let res = ctx
            .sqlite
            .conn()
            .and_then(|c| db::update_bot_config(&c, &ctx.security, u, ctx.now()));
        if let Err(e) = res {
            tracing::error!("Could not update the Telegram bot settings: {}", e);
        }
    }

    /// Web `initialize_bot_sync`: one bounded `getMe`. A token Telegram
    /// refuses is refused; when Telegram cannot be reached the token stays
    /// stored and the start goes ahead.
    pub async fn initialize(&self, ctx: &AppState, token: &Secret) -> (bool, String) {
        match self.api(ctx, token.clone()).get_me().await {
            Ok(me) => {
                let name = me
                    .get("username")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                Self::update_config(
                    ctx,
                    db::ConfigUpdate {
                        bot_username: Some(name.clone()),
                        ..Default::default()
                    },
                );
                tracing::info!("Telegram bot validated: @{}", name);
                (true, format!("Bot initialized successfully: @{}", name))
            }
            Err(TgError::Network(e)) => {
                tracing::warn!("Telegram token check could not reach Telegram: {}", e);
                (true, "Token stored (will validate on start)".into())
            }
            Err(e) => {
                tracing::warn!("Telegram refused the bot token check ({})", e);
                if matches!(e.code(), Some(401) | Some(404)) {
                    (
                        false,
                        "Telegram did not accept this bot token. Copy it again from BotFather and save it."
                            .into(),
                    )
                } else {
                    (
                        false,
                        "Telegram did not confirm the bot token. Try again in a few minutes, and if it keeps failing, copy the token again from BotFather."
                            .into(),
                    )
                }
            }
        }
    }

    /// Web `start_bot`: single flight; waits up to five seconds for polling.
    pub async fn start(&self, ctx: &Arc<AppState>) -> (bool, String) {
        let mut lc = self.lifecycle.lock().await;
        if let Some(run) = lc.as_ref() {
            if !run.join.is_finished() {
                return if self.is_running() {
                    (false, "Bot is already running".into())
                } else {
                    (false, BOT_BUSY_MESSAGE.into())
                };
            }
        }
        // Reap a finished task before replacing it.
        if let Some(old) = lc.take() {
            let _ = old.join.await;
        }
        let token = match Self::config(ctx) {
            Ok(c) => match c.token {
                Some(t) => t,
                None => return (false, "Bot token not configured".into()),
            },
            Err(e) => {
                tracing::error!("Could not read the Telegram bot settings: {}", e);
                return (
                    false,
                    "The bot could not be started. Check the server logs for the cause.".into(),
                );
            }
        };
        let gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.running.store(false, Ordering::SeqCst);
        let cancel = ctx.shutdown.child_token();
        let task = PollTask {
            ctx: Arc::downgrade(ctx),
            api: self.api(ctx, token),
            gen,
            cancel: cancel.clone(),
            timing: self.timing(),
            running: self.running.clone(),
            polls: self.polls.clone(),
        };
        let join = tokio::spawn(task.run());
        *lc = Some(PollRun { gen, cancel, join });
        drop(lc);

        let deadline = tokio::time::Instant::now() + BOT_START_WAIT;
        loop {
            if self.is_running() {
                return (true, "Bot started successfully".into());
            }
            let finished = {
                let lc = self.lifecycle.lock().await;
                lc.as_ref().map(|r| r.join.is_finished()).unwrap_or(true)
            };
            if finished || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.is_running() {
            return (true, "Bot started successfully".into());
        }
        (false, "Bot failed to start within timeout".into())
    }

    /// Cancel the task and wait for it (aborting after `BOT_STOP_JOIN`).
    async fn end_run(run: PollRun) {
        run.cancel.cancel();
        let mut join = run.join;
        if tokio::time::timeout(BOT_STOP_JOIN, &mut join)
            .await
            .is_err()
        {
            tracing::warn!("Telegram bot did not stop cleanly");
            join.abort();
            let _ = join.await;
        }
    }

    /// Web `stop_bot`, including a bot that is still connecting or backing off.
    pub async fn stop(&self, ctx: &AppState) -> (bool, String) {
        let mut lc = self.lifecycle.lock().await;
        let alive = lc.as_ref().map(|r| !r.join.is_finished()).unwrap_or(false);
        if !self.is_running() && !alive {
            if let Some(run) = lc.take() {
                let _ = run.join.await;
            }
            return (false, "Bot is not running".into());
        }
        if let Some(run) = lc.take() {
            let gen = run.gen;
            Self::end_run(run).await;
            if self.gen.load(Ordering::SeqCst) == gen {
                self.running.store(false, Ordering::SeqCst);
            }
        }
        self.running.store(false, Ordering::SeqCst);
        drop(lc);
        Self::update_config(
            ctx,
            db::ConfigUpdate {
                is_active: Some(false),
                ..Default::default()
            },
        );
        tracing::info!("Telegram bot stopped");
        (true, "Bot stopped successfully".into())
    }

    /// App exit: end the task without touching `is_active`, so the bot comes
    /// back on the next launch as the web's does.
    pub async fn shutdown(&self) {
        let run = self.lifecycle.lock().await.take();
        if let Some(run) = run {
            Self::end_run(run).await;
        }
        self.running.store(false, Ordering::SeqCst);
    }

    /// Web `is_bot_active`: the persisted flag start sets and stop clears.
    pub fn is_bot_active(ctx: &AppState) -> bool {
        Self::config(ctx).map(|c| c.is_active).unwrap_or(false)
    }

    /// Web `send_alert_sync`: Markdown, retried as plain text when Telegram
    /// cannot parse it; queued in `notification_queue` when it fails.
    pub async fn send_alert(&self, ctx: &AppState, telegram_id: i64, message: &str) -> bool {
        let token = match Self::config(ctx) {
            Ok(c) => c.token,
            Err(e) => {
                tracing::error!("Could not read the Telegram bot settings: {}", e);
                None
            }
        };
        let Some(token) = token else {
            tracing::error!("No Telegram bot token configured, queueing notification");
            Self::queue(ctx, telegram_id, message);
            return false;
        };
        let api = self.api(ctx, token);
        let mut res = api.send_message(telegram_id, message, true, None).await;
        if matches!(&res, Err(e) if e.is_parse_error()) {
            tracing::warn!(
                "Telegram could not parse the message formatting, sending as plain text"
            );
            res = api.send_message(telegram_id, message, false, None).await;
        }
        match res {
            Ok(_) => {
                tracing::info!("Telegram notification sent");
                true
            }
            Err(e) => {
                tracing::error!("Telegram notification failed: {}", e);
                Self::queue(ctx, telegram_id, message);
                false
            }
        }
    }

    fn queue(ctx: &AppState, telegram_id: i64, message: &str) {
        let res = ctx
            .sqlite
            .conn()
            .and_then(|c| db::add_notification(&c, telegram_id, message, 8, ctx.now()));
        if let Err(e) = res {
            tracing::error!("Could not queue a Telegram notification: {}", e);
        }
    }

    /// Web `send_order_alert` for one bus event.
    pub async fn send_order_alert(&self, ctx: &AppState, meta: &OrderMeta) {
        if !Self::is_bot_active(ctx) {
            tracing::debug!("Telegram bot is stopped; skipping order alert");
            return;
        }
        let Some(username) = crate::messaging::account_username(ctx) else {
            return;
        };
        let user = match ctx
            .sqlite
            .conn()
            .and_then(|c| db::get_user_by_username(&c, &username))
        {
            Ok(Some(u)) => u,
            Ok(None) => {
                tracing::debug!("No Telegram user linked for the account");
                return;
            }
            Err(e) => {
                tracing::error!("Could not read the linked Telegram user: {}", e);
                return;
            }
        };
        if !user.notifications_enabled {
            return;
        }
        let text = alerts::order_alert_text(
            Channel::Telegram,
            &meta.api_type,
            &meta.request_data,
            &meta.response_data,
            alerts::is_analyze(meta),
            &alerts::local_time(ctx),
        );
        self.send_alert(ctx, user.telegram_id, &text).await;
    }
}

/// One polling run.
struct PollTask {
    ctx: Weak<AppState>,
    api: BotApi,
    gen: u64,
    cancel: CancellationToken,
    timing: Timing,
    running: Arc<AtomicBool>,
    polls: Arc<AtomicU64>,
}

impl PollTask {
    /// Sleep unless cancelled; true when cancelled.
    async fn sleep_or_stop(&self, d: Duration) -> bool {
        tokio::select! {
            _ = self.cancel.cancelled() => true,
            _ = tokio::time::sleep(d) => false,
        }
    }

    async fn connect(&self) -> bool {
        let mut retry = 0u32;
        loop {
            let res = tokio::select! {
                _ = self.cancel.cancelled() => return false,
                r = self.api.delete_webhook() => r,
            };
            match res {
                Ok(_) => return true,
                Err(e) if e.is_network() => {
                    retry += 1;
                    tracing::warn!(
                        "Could not reach Telegram (attempt {}/{})",
                        retry,
                        START_MAX_RETRIES
                    );
                    if retry >= START_MAX_RETRIES {
                        tracing::error!(
                            "Telegram could not be reached. Check the internet connection, a firewall or DNS."
                        );
                        return false;
                    }
                    let delay = self.timing.start_backoff * 2u32.saturating_pow(retry);
                    if self.sleep_or_stop(delay).await {
                        return false;
                    }
                }
                Err(e) => {
                    tracing::error!("Telegram refused the bot connection: {}", e);
                    return false;
                }
            }
        }
    }

    async fn run(self) {
        if !self.connect().await {
            self.running.store(false, Ordering::SeqCst);
            return;
        }
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        if self.cancel.is_cancelled() {
            return;
        }
        let current = ctx.messaging.telegram.gen.load(Ordering::SeqCst) == self.gen;
        if current {
            self.running.store(true, Ordering::SeqCst);
            TelegramService::update_config(
                &ctx,
                db::ConfigUpdate {
                    is_active: Some(true),
                    ..Default::default()
                },
            );
            tracing::info!("Telegram bot started and is polling for updates");
        }
        drop(ctx);

        let mut offset = 0i64;
        let mut backoff = self.timing.poll_backoff;
        let mut bot = bot::Bot::new(self.api.clone());
        loop {
            let res = tokio::select! {
                _ = self.cancel.cancelled() => break,
                r = self.api.get_updates(offset, self.timing.poll_timeout_secs) => r,
            };
            self.polls.fetch_add(1, Ordering::SeqCst);
            match res {
                Ok(updates) => {
                    backoff = self.timing.poll_backoff;
                    for u in updates {
                        if let Some(id) = u.get("update_id").and_then(Value::as_i64) {
                            offset = offset.max(id + 1);
                        }
                        let Some(ctx) = self.ctx.upgrade() else {
                            return;
                        };
                        tokio::select! {
                            _ = self.cancel.cancelled() => return,
                            _ = bot.handle_update(&ctx, &u) => {}
                        }
                    }
                }
                Err(e) => {
                    match e.code() {
                        Some(401) | Some(404) => {
                            tracing::error!(
                                "Telegram no longer accepts the bot token; the bot has stopped."
                            );
                            break;
                        }
                        Some(409) => tracing::error!(
                            "Another copy of this bot is polling Telegram. Stop the other copy."
                        ),
                        _ => tracing::warn!("Telegram polling failed: {}. Retrying.", e),
                    }
                    if self.sleep_or_stop(backoff).await {
                        break;
                    }
                    backoff = (backoff * 2).min(self.timing.poll_backoff_max);
                }
            }
        }
        self.running.store(false, Ordering::SeqCst);
    }
}
