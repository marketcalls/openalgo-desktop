//! WhatsApp (web `services/whatsapp_bot_service.py` and
//! `services/whatsapp_alert_service.py`) on whatsapp-rust directly.
//!
//! Pairing: one owned task builds a client on a fresh in-memory session
//! store, streams `whatsapp_qr` / `whatsapp_pair_code` to the page, and once
//! WhatsApp confirms the login it closes the client, seals the session
//! snapshot into `whatsapp_config.session_blob`, emits `whatsapp_paired` and
//! `whatsapp_pair_status`, and starts the bot, as the web does.
//!
//! Bot: one owned task per start restores the session from the sealed blob
//! and keeps a client connected. whatsapp-rust reconnects dropped sockets by
//! itself; when its run loop ends without a logout the task rebuilds the
//! client with a capped backoff. The live session is saved back a minute
//! after each login, every five minutes when it changed, and at stop. A
//! logout by WhatsApp clears the stored session (unless a newer pairing
//! replaced it) and every surface then says to pair again. Slash commands
//! from the paired owner run on a worker task owned by the same run.

pub mod commands;
pub mod db;
pub mod http;
pub mod store;

use crate::events::OrderMeta;
use crate::messaging::alerts::{self, Channel};
use crate::messaging::format::redact_phone;
use crate::state::AppState;
use parking_lot::RwLock;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use store::SnapshotStore;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use whatsapp_rust::bot::Bot;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::{Event, EventKind, MessageExt};
use whatsapp_rust::transport::TokioWebSocketTransportFactory;
use whatsapp_rust::{Client, Jid, TokioRuntime};

pub const CONNECTING_MESSAGE: &str =
    "WhatsApp is still connecting. Check the status in a few seconds.";
pub const SEND_ABANDONED_ERROR: &str = "WhatsApp disconnected before this message was sent.";
pub const LOGGED_OUT_MESSAGE: &str = "WhatsApp logged this device out, so alerts cannot be sent. Pair it again from the /whatsapp page in OpenAlgo.";
pub const NOT_PAIRED_MESSAGE: &str = "Device not paired. Pair from /whatsapp first.";
pub const PAIR_TIMEOUT_MESSAGE: &str = "Pairing timed out — please try again";
/// Most recipients one send reaches (web `MAX_RECIPIENTS`).
pub const MAX_RECIPIENTS: usize = 5;
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_JOIN: Duration = Duration::from_secs(10);
const START_WAIT: Duration = Duration::from_secs(15);

/// Connection settings, adjustable by tests (which point the socket at a
/// local closed port and block HTTP so nothing reaches WhatsApp).
#[derive(Debug, Clone)]
pub struct ConnSettings {
    pub ws_url: Option<String>,
    pub http_blocked: bool,
    pub version: Option<(u32, u32, u32)>,
    pub pair_timeout: Duration,
    pub reconnect_base: Duration,
    pub reconnect_max: Duration,
    pub first_save: Duration,
    pub save_interval: Duration,
}

impl Default for ConnSettings {
    fn default() -> Self {
        Self {
            ws_url: None,
            http_blocked: false,
            version: None,
            pair_timeout: Duration::from_secs(300),
            reconnect_base: Duration::from_secs(2),
            reconnect_max: Duration::from_secs(60),
            first_save: Duration::from_secs(60),
            save_interval: Duration::from_secs(300),
        }
    }
}

/// The pairing state the page polls (`/whatsapp/pair/status`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PairState {
    pub status: String,
    pub qr_data_url: Option<String>,
    pub pair_code: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub paired_at: Option<String>,
}

impl Default for PairState {
    fn default() -> Self {
        Self {
            status: "idle".into(),
            qr_data_url: None,
            pair_code: None,
            error: None,
            started_at: None,
            paired_at: None,
        }
    }
}

impl PairState {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// What the pairing task learns.
#[derive(Debug, Clone)]
pub enum PairEvent {
    Qr(String),
    Code(String),
    Paired {
        own_jid: Option<String>,
        own_phone: Option<String>,
    },
    Failed(String),
}

fn iso_now() -> String {
    chrono::Utc::now()
        .naive_utc()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

/// The pairing state machine: apply an event, return the Socket.IO events
/// to emit (names and payloads as the web's).
pub fn apply_pair_event(state: &mut PairState, ev: PairEvent) -> Vec<(&'static str, Value)> {
    match ev {
        PairEvent::Qr(code) => {
            let url = match crate::security::totp::qr_png_base64(&code) {
                Ok(b) => Some(format!("data:image/png;base64,{}", b)),
                Err(e) => {
                    tracing::error!("Could not draw the WhatsApp QR code: {}", e);
                    None
                }
            };
            state.status = "awaiting_scan".into();
            state.qr_data_url = url.clone();
            vec![("whatsapp_qr", json!({"data_url": url}))]
        }
        PairEvent::Code(code) => {
            state.status = "awaiting_scan".into();
            state.pair_code = Some(code.clone());
            vec![("whatsapp_pair_code", json!({"code": code}))]
        }
        PairEvent::Paired { own_jid, own_phone } => {
            state.status = "paired".into();
            state.paired_at = Some(iso_now());
            vec![
                (
                    "whatsapp_paired",
                    json!({"own_phone": own_phone, "own_jid": own_jid}),
                ),
                ("whatsapp_pair_status", state.to_json()),
            ]
        }
        PairEvent::Failed(err) => {
            state.status = "failed".into();
            state.error = Some(err);
            vec![("whatsapp_pair_status", state.to_json())]
        }
    }
}

/// What a client's event handler forwards to its owning task.
#[derive(Debug)]
enum Raw {
    Qr(String),
    Code(String),
    Connected,
    LoggedOut,
    Message {
        is_from_me: bool,
        is_group: bool,
        sender: String,
        chat: String,
        text: String,
    },
}

/// Who may drive the bot: the paired owner's own messages (web: only
/// `is_from_me`), and never in a group or broadcast chat, so account data is
/// not answered where others read it.
pub fn command_allowed(is_from_me: bool, is_group: bool, chat: &str) -> bool {
    is_from_me
        && !is_group
        && !chat.ends_with("@g.us")
        && !chat.ends_with("@broadcast")
        && !chat.ends_with("@newsletter")
}

/// Web `normalize_phone`: digits only, 7 to 15 of them; floats and booleans
/// are refused.
pub fn normalize_phone(raw: &Value) -> String {
    let text = match raw {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        _ => return String::new(),
    };
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    if (7..=15).contains(&digits.len()) {
        digits
    } else {
        String::new()
    }
}

pub fn phone_to_jid(digits: &str) -> String {
    format!("{}@s.whatsapp.net", digits)
}

pub fn jid_to_phone(jid: &str) -> String {
    if !jid.contains("@s.whatsapp.net") {
        return String::new();
    }
    jid.split('@').next().unwrap_or("").to_string()
}

/// Per-recipient result of a send (web `send_sync` report).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub sent: Vec<String>,
    pub failed: Vec<Value>,
    pub skipped: usize,
}

impl Report {
    fn failed_all(error: &str) -> Self {
        Self {
            sent: vec![],
            failed: vec![json!({"to": "<bot>", "error": error})],
            skipped: 0,
        }
    }
}

struct Run {
    cancel: CancellationToken,
    join: JoinHandle<()>,
}

impl Run {
    fn alive(&self) -> bool {
        !self.join.is_finished()
    }
    async fn end(self) {
        self.cancel.cancel();
        let mut join = self.join;
        if tokio::time::timeout(STOP_JOIN, &mut join).await.is_err() {
            tracing::warn!("WhatsApp did not stop in time; ending it");
            join.abort();
            let _ = join.await;
        }
    }
}

#[derive(Default)]
struct Tasks {
    pair: Option<Run>,
    bot: Option<Run>,
}

pub struct WhatsAppService {
    tasks: tokio::sync::Mutex<Tasks>,
    pair: parking_lot::Mutex<PairState>,
    running: AtomicBool,
    logged_out: AtomicBool,
    client: RwLock<Option<Arc<Client>>>,
    settings: RwLock<ConnSettings>,
    /// Clients built so far (pairing and bot), for tests.
    builds: AtomicU64,
}

impl Default for WhatsAppService {
    fn default() -> Self {
        Self::new()
    }
}

impl WhatsAppService {
    pub fn new() -> Self {
        Self {
            tasks: tokio::sync::Mutex::new(Tasks::default()),
            pair: parking_lot::Mutex::new(PairState::default()),
            running: AtomicBool::new(false),
            logged_out: AtomicBool::new(false),
            client: RwLock::new(None),
            settings: RwLock::new(ConnSettings::default()),
            builds: AtomicU64::new(0),
        }
    }

    pub fn set_settings(&self, s: ConnSettings) {
        *self.settings.write() = s;
    }

    pub fn builds(&self) -> u64 {
        self.builds.load(Ordering::SeqCst)
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn pair_state(&self) -> PairState {
        self.pair.lock().clone()
    }

    pub async fn tasks_alive(&self) -> (bool, bool) {
        let t = self.tasks.lock().await;
        (
            t.pair.as_ref().map(Run::alive).unwrap_or(false),
            t.bot.as_ref().map(Run::alive).unwrap_or(false),
        )
    }

    pub fn config(ctx: &AppState) -> db::WaConfig {
        ctx.sqlite
            .conn()
            .and_then(|c| db::get_config(&c))
            .unwrap_or_else(|e| {
                tracing::error!("Could not read the WhatsApp settings: {}", e);
                db::WaConfig::default()
            })
    }

    pub fn is_paired(ctx: &AppState) -> bool {
        Self::config(ctx).is_paired
    }

    /// Web `is_ready`: paired and connected.
    pub fn is_ready(&self, ctx: &AppState) -> bool {
        self.is_running() && self.client.read().is_some() && Self::is_paired(ctx)
    }

    /// Web `unavailable_reason`.
    pub fn unavailable_reason(&self) -> Option<&'static str> {
        (self.logged_out.load(Ordering::SeqCst) && !self.is_running()).then_some(LOGGED_OUT_MESSAGE)
    }

    /// The `whatsapp_status` payload.
    pub fn status_payload(&self, ctx: &AppState) -> Value {
        json!({
            "is_running": self.is_running(),
            "is_paired": Self::is_paired(ctx),
            "status_message": self.unavailable_reason(),
        })
    }

    async fn emit_status(&self, ctx: &AppState) {
        crate::messaging::emit(ctx, "whatsapp_status", self.status_payload(ctx)).await;
    }

    async fn build(
        &self,
        ctx: &AppState,
        store: Arc<SnapshotStore>,
        tx: mpsc::Sender<Raw>,
        phone: Option<String>,
    ) -> Result<Bot, String> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        let s = self.settings.read().clone();
        let mut factory = TokioWebSocketTransportFactory::new();
        if let Some(u) = &s.ws_url {
            factory = factory.with_url(u.clone());
        }
        let kinds = [
            EventKind::PairingQrCode,
            EventKind::PairingCode,
            EventKind::Connected,
            EventKind::LoggedOut,
            EventKind::Message,
        ];
        let mut b = Bot::builder()
            .with_backend_arc(store)
            .with_transport_factory(factory)
            .with_http_client(http::ReqwestHttp::new(ctx.http.clone(), s.http_blocked))
            .with_runtime(TokioRuntime)
            .skip_history_sync()
            .on_event_for(&kinds, move |ev: Arc<Event>, _c| {
                let raw = match &*ev {
                    Event::PairingQrCode { code, .. } => Some(Raw::Qr(code.clone())),
                    Event::PairingCode { code, .. } => Some(Raw::Code(code.clone())),
                    Event::Connected(_) => Some(Raw::Connected),
                    Event::LoggedOut(_) => Some(Raw::LoggedOut),
                    Event::Message(m, info) => Some(Raw::Message {
                        is_from_me: info.source.is_from_me,
                        is_group: info.source.is_group,
                        sender: info.source.sender.to_non_ad_string(),
                        chat: info.source.chat.to_non_ad_string(),
                        text: m.text_content().unwrap_or("").to_string(),
                    }),
                    _ => None,
                };
                let tx = tx.clone();
                async move {
                    if let Some(r) = raw {
                        if tx.try_send(r).is_err() {
                            tracing::debug!("WhatsApp event dropped: queue full");
                        }
                    }
                }
            });
        if let Some(v) = s.version {
            b = b.with_version(v);
        }
        if let Some(p) = phone {
            b = b.with_pair_code(PairCodeOptions::for_phone(p));
        }
        b.build().await.map_err(|e| e.to_string())
    }

    // ------------------------------------------------------------- pairing

    /// Web `start_pair`. Non-blocking: the QR and pair code arrive over
    /// Socket.IO and `/whatsapp/pair/status`.
    pub async fn start_pair(
        &self,
        ctx: &Arc<AppState>,
        phone: Option<String>,
        owner_user_id: Option<i64>,
        owner_username: Option<String>,
    ) -> (bool, String) {
        let mut t = self.tasks.lock().await;
        {
            let p = self.pair.lock();
            if matches!(p.status.as_str(), "starting" | "awaiting_scan")
                && t.pair.as_ref().map(Run::alive).unwrap_or(false)
            {
                return (false, "Pairing already in progress".into());
            }
        }
        if self.is_running() {
            return (
                false,
                "Bot is currently connected. Stop it before re-pairing.".into(),
            );
        }
        if let Some(old) = t.pair.take() {
            old.end().await;
        }
        *self.pair.lock() = PairState {
            status: "starting".into(),
            started_at: Some(iso_now()),
            ..Default::default()
        };
        let cancel = ctx.shutdown.child_token();
        let join = tokio::spawn(pair_task(
            Arc::downgrade(ctx),
            phone,
            owner_user_id,
            owner_username,
            cancel.clone(),
        ));
        t.pair = Some(Run { cancel, join });
        (true, "Pairing started. Watch for QR or pair code.".into())
    }

    async fn pair_event(&self, ctx: &AppState, ev: PairEvent) {
        let emits = {
            let mut p = self.pair.lock();
            apply_pair_event(&mut p, ev)
        };
        for (name, payload) in emits {
            crate::messaging::emit(ctx, name, payload).await;
        }
    }

    // ----------------------------------------------------------------- bot

    /// Web `start_bot`: single flight; waits for the client to come up.
    pub async fn start_bot(&self, ctx: &Arc<AppState>) -> (bool, String) {
        let mut t = self.tasks.lock().await;
        if let Some(run) = t.bot.as_ref() {
            if run.alive() {
                return if self.is_running() && self.client.read().is_some() {
                    (true, "Bot already running".into())
                } else {
                    (false, CONNECTING_MESSAGE.into())
                };
            }
        }
        if let Some(old) = t.bot.take() {
            old.end().await;
        }
        let loaded = ctx
            .sqlite
            .conn()
            .and_then(|c| db::load_session(&c, &ctx.security));
        let (snapshot, stored) = match loaded {
            Ok(Some(s)) => s,
            Ok(None) => {
                return (
                    false,
                    self.unavailable_reason().unwrap_or(NOT_PAIRED_MESSAGE).into(),
                )
            }
            Err(e) => {
                tracing::error!("Could not open the stored WhatsApp session: {}", e);
                return (
                    false,
                    "The saved WhatsApp session could not be opened. Pair the device again from this page."
                        .into(),
                );
            }
        };
        let (ready_tx, ready_rx) = oneshot::channel();
        let cancel = ctx.shutdown.child_token();
        let join = tokio::spawn(bot_task(
            Arc::downgrade(ctx),
            snapshot,
            stored,
            cancel.clone(),
            ready_tx,
        ));
        t.bot = Some(Run { cancel, join });
        drop(t);
        match tokio::time::timeout(START_WAIT, ready_rx).await {
            Ok(Ok(true)) => (true, "Bot started".into()),
            Ok(_) => (
                false,
                "WhatsApp could not be started. Check the server logs for the cause.".into(),
            ),
            Err(_) => (
                false,
                "WhatsApp did not come up within 15 seconds. Check the status in a few seconds."
                    .into(),
            ),
        }
    }

    /// Web `stop_bot`: the session is saved one last time on the way out.
    pub async fn stop_bot(&self, ctx: &AppState) -> (bool, String) {
        let was_running = self.is_running();
        let run = self.tasks.lock().await.bot.take();
        match run {
            Some(r) => {
                let alive = r.alive();
                r.end().await;
                if !alive && !was_running {
                    return (true, "Bot is not running".into());
                }
            }
            None if !was_running => return (true, "Bot is not running".into()),
            None => {}
        }
        self.running.store(false, Ordering::SeqCst);
        *self.client.write() = None;
        let _ = ctx.sqlite.conn().and_then(|c| db::set_active(&c, false));
        tracing::info!("WhatsApp bot stopped");
        (
            true,
            if was_running {
                "Bot stopped".into()
            } else {
                "Bot is not running".into()
            },
        )
    }

    /// Web `unlink`: stop, then forget the session.
    pub async fn unlink(&self, ctx: &AppState) -> (bool, String) {
        self.stop_bot(ctx).await;
        let ok = ctx
            .sqlite
            .conn()
            .and_then(|c| db::clear_session(&c))
            .unwrap_or(false);
        self.logged_out.store(false, Ordering::SeqCst);
        crate::messaging::emit(
            ctx,
            "whatsapp_status",
            json!({"is_running": false, "is_paired": false, "status_message": null}),
        )
        .await;
        (
            ok,
            if ok {
                "Device unlinked".into()
            } else {
                "Failed to unlink".into()
            },
        )
    }

    /// App exit: end pairing and the bot (the bot saves its session).
    pub async fn shutdown(&self) {
        let (pair, bot) = {
            let mut t = self.tasks.lock().await;
            (t.pair.take(), t.bot.take())
        };
        if let Some(p) = pair {
            p.end().await;
        }
        if let Some(b) = bot {
            b.end().await;
        }
        self.running.store(false, Ordering::SeqCst);
        *self.client.write() = None;
    }

    // ---------------------------------------------------------------- send

    /// Web `send_sync`. `to` empty means the paired device itself.
    pub async fn send(&self, ctx: &AppState, to: &[String], text: &str) -> Report {
        let client = self.client.read().clone();
        let Some(client) = client.filter(|_| self.is_running()) else {
            tracing::warn!("WhatsApp send: bot not running, dropping message");
            return Report::failed_all(self.unavailable_reason().unwrap_or("Bot not connected"));
        };
        let mut report = Report::default();
        let mut targets: Vec<String> = to.iter().filter(|s| !s.is_empty()).cloned().collect();
        if targets.is_empty() {
            let own = Self::config(ctx)
                .own_jid
                .or_else(|| client.get_pn().map(|j| j.to_non_ad_string()));
            match own {
                Some(j) => targets.push(j),
                None => {
                    report
                        .failed
                        .push(json!({"to": "<self>", "error": "No recipient resolved"}));
                    return report;
                }
            }
        }
        if targets.len() > MAX_RECIPIENTS {
            report.skipped = targets.len() - MAX_RECIPIENTS;
            targets.truncate(MAX_RECIPIENTS);
        }
        for jid_s in targets {
            let jid: Jid = match jid_s.parse() {
                Ok(j) => j,
                Err(_) => {
                    report
                        .failed
                        .push(json!({"to": jid_s, "error": "This is not a valid WhatsApp number."}));
                    continue;
                }
            };
            match tokio::time::timeout(SEND_TIMEOUT, client.send_text(jid, text)).await {
                Ok(Ok(_)) => {
                    tracing::info!("WhatsApp message sent to {}", redact_phone(&jid_s));
                    report.sent.push(jid_s);
                }
                Ok(Err(e)) => {
                    tracing::error!("WhatsApp send to {} failed: {}", redact_phone(&jid_s), e);
                    report.failed.push(json!({"to": jid_s,
                        "error": "WhatsApp did not accept the message. Check the number and that the device is still linked."}));
                }
                Err(_) => {
                    report.failed.push(json!({"to": jid_s, "error": "send timeout"}));
                }
            }
        }
        report
    }

    /// Web `send_alert_sync`: true when at least one recipient got it;
    /// dropped (not queued) while the bot cannot send.
    pub async fn send_alert(&self, ctx: &AppState, to: &[String], text: &str) -> bool {
        if !self.is_ready(ctx) {
            tracing::debug!("WhatsApp alert dropped: bot not paired or connected");
            return false;
        }
        !self.send(ctx, to, text).await.sent.is_empty()
    }

    /// Web `send_order_alert`: to the paired owner, plus a linked user.
    pub async fn send_order_alert(&self, ctx: &AppState, meta: &OrderMeta) {
        let Some(username) = crate::messaging::account_username(ctx) else {
            return;
        };
        let text = alerts::order_alert_text(
            Channel::WhatsApp,
            &meta.api_type,
            &meta.request_data,
            &meta.response_data,
            alerts::is_analyze(meta),
            &alerts::local_time(ctx),
        );
        let cfg = Self::config(ctx);
        if cfg.is_paired && cfg.owner_username.as_deref() == Some(username.as_str()) {
            self.send_alert(ctx, &[], &text).await;
        }
        let linked = ctx
            .sqlite
            .conn()
            .and_then(|c| db::get_user_by_username(&c, &username))
            .ok()
            .flatten();
        if let Some(u) = linked.filter(|u| u.notifications_enabled) {
            self.send_alert(ctx, &[u.whatsapp_jid], &text).await;
        }
    }

    /// Fan a message out to linked users (web `send_broadcast_alert`):
    /// (queued, skipped). Sends run on a task owned by the app context.
    pub fn broadcast(&self, ctx: &Arc<AppState>, message: &str, broker: Option<String>, notif: Option<bool>) -> (usize, usize) {
        let users = ctx
            .sqlite
            .conn()
            .and_then(|c| db::all_users(&c, broker.as_deref(), notif))
            .unwrap_or_default();
        let mut targets = Vec::new();
        let mut skipped = 0;
        for u in users {
            if u.notifications_enabled {
                targets.push(u.whatsapp_jid);
            } else {
                skipped += 1;
            }
        }
        let queued = targets.len();
        let weak = Arc::downgrade(ctx);
        let msg = message.to_string();
        ctx.spawn(async move {
            for t in targets {
                let Some(ctx) = weak.upgrade() else { return };
                ctx.messaging.whatsapp.send_alert(&ctx, &[t], &msg).await;
            }
        });
        (queued, skipped)
    }
}

// ------------------------------------------------------------------ tasks

async fn pair_task(
    ctx: Weak<AppState>,
    phone: Option<String>,
    owner_user_id: Option<i64>,
    owner_username: Option<String>,
    cancel: CancellationToken,
) {
    let Some(c) = ctx.upgrade() else { return };
    let svc = &c.messaging.whatsapp;
    let store = Arc::new(SnapshotStore::new());
    let (tx, mut rx) = mpsc::channel::<Raw>(64);
    let bot = match svc.build(&c, store.clone(), tx, phone).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("WhatsApp pairing could not start: {}", e);
            svc.pair_event(
                &c,
                PairEvent::Failed("WhatsApp pairing could not start. Try again.".into()),
            )
            .await;
            return;
        }
    };
    let timeout = svc.settings.read().pair_timeout;
    drop(c);
    let mut handle = bot.spawn();
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let outcome: Option<PairEvent> = loop {
        tokio::select! {
            _ = cancel.cancelled() => break None,
            _ = &mut deadline => break Some(PairEvent::Failed(PAIR_TIMEOUT_MESSAGE.into())),
            _ = &mut handle => break Some(PairEvent::Failed(
                "WhatsApp closed the pairing connection. Try again.".into())),
            ev = rx.recv() => {
                let Some(ev) = ev else { continue };
                let Some(c) = ctx.upgrade() else { break None };
                match ev {
                    Raw::Qr(code) => c.messaging.whatsapp.pair_event(&c, PairEvent::Qr(code)).await,
                    Raw::Code(code) => c.messaging.whatsapp.pair_event(&c, PairEvent::Code(code)).await,
                    Raw::Connected => {
                        let client = handle.client();
                        if client.is_logged_in() || store.has_paired_device() {
                            let own_jid = client.get_pn().map(|j| j.to_non_ad_string());
                            let own_phone = own_jid.as_deref().map(jid_to_phone).filter(|p| !p.is_empty());
                            break Some(PairEvent::Paired { own_jid, own_phone });
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    // Close the pairing client (flushes its device state into the store).
    if tokio::time::timeout(STOP_JOIN, handle.client().disconnect())
        .await
        .is_err()
    {
        tracing::warn!("WhatsApp pairing client did not close in time");
    }
    handle.abort();
    let Some(outcome) = outcome else { return };
    let Some(c) = ctx.upgrade() else { return };
    let svc = &c.messaging.whatsapp;
    let PairEvent::Paired { own_jid, own_phone } = outcome else {
        svc.pair_event(&c, outcome).await;
        return;
    };
    let saved = store
        .export()
        .map_err(|e| e.to_string())
        .and_then(|snap| {
            let conn = c.sqlite.conn().map_err(|e| e.to_string())?;
            db::save_session(
                &conn,
                &c.security,
                &snap,
                &db::Owner {
                    own_jid: own_jid.as_deref(),
                    own_phone: own_phone.as_deref(),
                    owner_user_id,
                    owner_username: owner_username.as_deref(),
                },
                c.now(),
            )
            .map_err(|e| e.to_string())
        });
    if let Err(e) = saved {
        tracing::error!("Saving the WhatsApp session failed: {}", e);
        svc.pair_event(
            &c,
            PairEvent::Failed("The WhatsApp session could not be saved. Try pairing again.".into()),
        )
        .await;
        return;
    }
    svc.logged_out.store(false, Ordering::SeqCst);
    tracing::info!(
        "WhatsApp device paired ({})",
        own_phone.as_deref().map(redact_phone).unwrap_or_default()
    );
    svc.pair_event(&c, PairEvent::Paired { own_jid, own_phone })
        .await;
    // Auto-start the bot, on its own task so this pairing task can finish.
    let weak = Arc::downgrade(&c);
    c.spawn(async move {
        if let Some(c) = weak.upgrade() {
            let (ok, msg) = c.messaging.whatsapp.start_bot(&c).await;
            tracing::info!("WhatsApp bot auto-start after pairing: {} ({})", ok, msg);
        }
    });
}

enum RunEnd {
    Stop,
    LoggedOut,
    Dropped,
}

struct Saver {
    store: Arc<SnapshotStore>,
    stored: String,
    version: u64,
    refused: bool,
}

impl Saver {
    /// Write the live session back when it changed (web `_save_session`).
    async fn save(&mut self, ctx: &AppState, require_login: bool, client: &Client) {
        if self.refused || self.store.version() == self.version {
            return;
        }
        if require_login && !client.is_logged_in() {
            return;
        }
        if !self.store.has_paired_device() {
            return;
        }
        let v = self.store.version();
        let store = self.store.clone();
        let snap = match tokio::task::spawn_blocking(move || store.export()).await {
            Ok(Ok(s)) => s,
            _ => {
                tracing::error!("Could not save the WhatsApp session; trying again later");
                return;
            }
        };
        let res = ctx
            .sqlite
            .conn()
            .and_then(|c| db::refresh_session(&c, &ctx.security, &snap, &self.stored));
        match res {
            Ok(Some(ct)) => {
                self.stored = ct;
                self.version = v;
                tracing::debug!("WhatsApp session saved");
            }
            Ok(None) => {
                tracing::info!(
                    "WhatsApp session not saved: the device is no longer paired with this session"
                );
                self.refused = true;
            }
            Err(e) => tracing::error!("Could not save the WhatsApp session: {}", e),
        }
    }
}

async fn bot_task(
    ctx: Weak<AppState>,
    snapshot: Vec<u8>,
    stored: String,
    cancel: CancellationToken,
    ready: oneshot::Sender<bool>,
) {
    let mut ready = Some(ready);
    let store = match SnapshotStore::import(&snapshot) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::error!("The stored WhatsApp session could not be read: {}", e);
            if let Some(r) = ready.take() {
                let _ = r.send(false);
            }
            return;
        }
    };
    drop(snapshot);
    let mut saver = Saver {
        version: store.version(),
        store: store.clone(),
        stored,
        refused: false,
    };
    let base = ctx
        .upgrade()
        .map(|c| c.messaging.whatsapp.settings.read().clone())
        .unwrap_or_default();
    let mut backoff = base.reconnect_base;
    loop {
        let Some(c) = ctx.upgrade() else { return };
        let svc = &c.messaging.whatsapp;
        let (tx, mut rx) = mpsc::channel::<Raw>(256);
        let bot = match svc.build(&c, store.clone(), tx, None).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("WhatsApp client could not be built: {}", e);
                if let Some(r) = ready.take() {
                    let _ = r.send(false);
                    return;
                }
                drop(c);
                if sleep_or_stop(&cancel, backoff).await {
                    return;
                }
                backoff = (backoff * 2).min(base.reconnect_max);
                continue;
            }
        };
        let mut handle = bot.spawn();
        let client = handle.client();
        *svc.client.write() = Some(client.clone());
        svc.running.store(true, Ordering::SeqCst);
        svc.logged_out.store(false, Ordering::SeqCst);
        let _ = c.sqlite.conn().and_then(|cn| db::set_active(&cn, true));
        svc.emit_status(&c).await;
        if let Some(r) = ready.take() {
            let _ = r.send(true);
        }
        tracing::info!("WhatsApp bot connecting");

        // Slash commands run one at a time on a worker owned by this run.
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<(String, String, String)>(32);
        let wctx = ctx.clone();
        let worker: JoinHandle<()> = tokio::spawn(async move {
            while let Some((chat, sender, text)) = cmd_rx.recv().await {
                let Some(c) = wctx.upgrade() else { return };
                commands::dispatch(&c, &chat, &sender, &text).await;
            }
        });
        drop(c);

        let s = base.clone();
        let mut next_save = tokio::time::Instant::now() + s.first_save;
        let end = loop {
            tokio::select! {
                _ = cancel.cancelled() => break RunEnd::Stop,
                _ = &mut handle => break RunEnd::Dropped,
                ev = rx.recv() => {
                    let Some(ev) = ev else { continue };
                    let Some(c) = ctx.upgrade() else { break RunEnd::Stop };
                    match ev {
                        Raw::Connected => {
                            backoff = s.reconnect_base;
                            next_save = next_save.min(tokio::time::Instant::now() + s.first_save);
                            if let Some(own) = client.get_pn() {
                                let jid = own.to_non_ad_string();
                                let _ = c.sqlite.conn().and_then(|cn| {
                                    db::persist_owner_identity(&cn, &jid, &jid_to_phone(&jid))
                                });
                            }
                            let restored = !c.messaging.whatsapp.running.swap(true, Ordering::SeqCst);
                            if restored {
                                let _ = c.sqlite.conn().and_then(|cn| db::set_active(&cn, true));
                                tracing::info!("WhatsApp connection is back");
                                c.messaging.whatsapp.emit_status(&c).await;
                            }
                        }
                        Raw::LoggedOut => break RunEnd::LoggedOut,
                        Raw::Message { is_from_me, is_group, sender, chat, text } => {
                            let text = text.trim().to_string();
                            if !text.starts_with('/') {
                                continue;
                            }
                            if !command_allowed(is_from_me, is_group, &chat) {
                                tracing::debug!("WhatsApp command from someone other than the owner ignored");
                                continue;
                            }
                            if cmd_tx.try_send((chat, sender, text)).is_err() {
                                tracing::warn!("WhatsApp command dropped: too many waiting");
                            }
                        }
                        _ => {}
                    }
                }
                _ = tokio::time::sleep_until(next_save) => {
                    if let Some(c) = ctx.upgrade() {
                        saver.save(&c, true, &client).await;
                    }
                    next_save = tokio::time::Instant::now() + s.save_interval;
                }
            }
        };
        drop(cmd_tx);
        worker.abort();
        let _ = worker.await;
        let Some(c) = ctx.upgrade() else {
            handle.abort();
            return;
        };
        let svc = &c.messaging.whatsapp;
        *svc.client.write() = None;
        match end {
            RunEnd::Stop => {
                // Save while connected, then close (which flushes the device
                // state), then save what the close flushed.
                saver.save(&c, true, &client).await;
                if tokio::time::timeout(STOP_JOIN, client.disconnect()).await.is_err() {
                    tracing::warn!("WhatsApp client did not close in time");
                }
                handle.abort();
                saver.save(&c, false, &client).await;
                svc.running.store(false, Ordering::SeqCst);
                let _ = c.sqlite.conn().and_then(|cn| db::set_active(&cn, false));
                svc.emit_status(&c).await;
                return;
            }
            RunEnd::LoggedOut => {
                handle.abort();
                svc.running.store(false, Ordering::SeqCst);
                let cleared = c
                    .sqlite
                    .conn()
                    .and_then(|cn| db::clear_rejected(&cn, &saver.stored))
                    .unwrap_or(false);
                if cleared || !saver.refused {
                    svc.logged_out.store(true, Ordering::SeqCst);
                    tracing::warn!("{}", LOGGED_OUT_MESSAGE);
                } else {
                    tracing::info!("WhatsApp logged out a session that a new pairing has replaced");
                }
                let _ = c.sqlite.conn().and_then(|cn| db::set_active(&cn, false));
                svc.emit_status(&c).await;
                return;
            }
            RunEnd::Dropped => {
                handle.abort();
                svc.running.store(false, Ordering::SeqCst);
                svc.emit_status(&c).await;
                tracing::warn!("WhatsApp connection ended; reconnecting");
                drop(c);
                if sleep_or_stop(&cancel, backoff).await {
                    if let Some(c) = ctx.upgrade() {
                        let _ = c.sqlite.conn().and_then(|cn| db::set_active(&cn, false));
                    }
                    return;
                }
                backoff = (backoff * 2).min(base.reconnect_max);
            }
        }
    }
}

async fn sleep_or_stop(cancel: &CancellationToken, d: Duration) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => true,
        _ = tokio::time::sleep(d) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_state_machine_and_events() {
        let mut s = PairState {
            status: "starting".into(),
            started_at: Some("t".into()),
            ..Default::default()
        };
        let e = apply_pair_event(&mut s, PairEvent::Qr("2@abc,def".into()));
        assert_eq!(s.status, "awaiting_scan");
        assert_eq!(e[0].0, "whatsapp_qr");
        let url = e[0].1["data_url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        assert_eq!(s.qr_data_url.as_deref(), Some(url));
        let e = apply_pair_event(&mut s, PairEvent::Code("ABCD1234".into()));
        assert_eq!(e, vec![("whatsapp_pair_code", json!({"code": "ABCD1234"}))]);
        let e = apply_pair_event(
            &mut s,
            PairEvent::Paired {
                own_jid: Some("91@s.whatsapp.net".into()),
                own_phone: Some("91".into()),
            },
        );
        assert_eq!(s.status, "paired");
        assert!(s.paired_at.is_some());
        assert_eq!(e[0], ("whatsapp_paired", json!({"own_phone": "91", "own_jid": "91@s.whatsapp.net"})));
        assert_eq!(e[1].0, "whatsapp_pair_status");
        assert_eq!(e[1].1["status"], "paired");
        let mut f = PairState::default();
        let e = apply_pair_event(&mut f, PairEvent::Failed(PAIR_TIMEOUT_MESSAGE.into()));
        assert_eq!(e[0].1["status"], "failed");
        assert_eq!(e[0].1["error"], PAIR_TIMEOUT_MESSAGE);
        let keys: Vec<_> = PairState::default().to_json().as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys.len(), 6);
    }

    #[test]
    fn phones_and_command_gate() {
        assert_eq!(normalize_phone(&json!("+91 98765-43210")), "919876543210");
        assert_eq!(normalize_phone(&json!(919876543210u64)), "919876543210");
        assert_eq!(normalize_phone(&json!(919876543210.0)), "");
        assert_eq!(normalize_phone(&json!(true)), "");
        assert_eq!(normalize_phone(&json!("123")), "");
        assert_eq!(phone_to_jid("91"), "91@s.whatsapp.net");
        assert_eq!(jid_to_phone("91@s.whatsapp.net"), "91");
        assert_eq!(jid_to_phone("1203@g.us"), "");
        assert!(command_allowed(true, false, "91@s.whatsapp.net"));
        assert!(!command_allowed(false, false, "91@s.whatsapp.net"));
        assert!(!command_allowed(true, true, "1203@g.us"));
        assert!(!command_allowed(true, false, "1203@g.us"));
        assert!(!command_allowed(true, false, "status@broadcast"));
    }
}
