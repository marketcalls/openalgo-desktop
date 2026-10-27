//! Request monitoring: traffic log, API latency, IP bans, 404 and invalid
//! API key tracking (web `utils/traffic_logger.py`, `utils/latency_monitor.py`,
//! `utils/security_middleware.py`).
//!
//! The middleware never writes to the database on the request path. It
//! sends a small record over a bounded channel to one writer task owned by
//! the app context; when the channel is full the record is dropped and
//! counted. Bans are checked against an in-memory copy of `ip_bans`,
//! refreshed by the writer and after every ban change.
//!
//! What is recorded: method, path without its query string, status, time
//! taken, client address and Host header. Never a request or response body,
//! a query string, or an API key. For `/api/v1` calls and the orders a
//! trader sends from a page the latency recorder reads only `symbol` from
//! the request and `orderid` / `request_id` / `message` from the response,
//! and takes the broker time from `services::latency_probe`.

use crate::db::sqlite::monitor::{self as store, LatencyRow, TrafficRow};
use crate::db::sqlite::webui::{self, SecuritySettings};
use crate::server::envelope::EarlyReply;
use crate::server::middleware::client_ip;
use crate::services::latency_probe::{self, BrokerTime};
use crate::state::AppState;
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Records waiting for the writer; more are dropped (and counted).
pub const CHANNEL_CAP: usize = 4_096;
/// Largest response body the latency recorder reads.
const MAX_SCAN_BYTES: u64 = 256 * 1024;
/// How often the writer purges old rows and refreshes the ban list.
const HOUSEKEEPING: Duration = Duration::from_secs(600);
const BAN_REFRESH: Duration = Duration::from_secs(30);
/// How long the writer's last save at shutdown may take.
const FINAL_SAVE: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum Record {
    Traffic(TrafficRow),
    Latency(LatencyRow),
    NotFound { ip: String, path: String },
    InvalidKey { ip: String },
}

#[derive(Default)]
struct BanCache {
    /// address -> expiry (None = permanent)
    bans: HashMap<String, Option<DateTime<Utc>>>,
}

pub struct Monitor {
    tx: mpsc::Sender<Record>,
    rx: Mutex<Option<mpsc::Receiver<Record>>>,
    bans: RwLock<BanCache>,
    settings: RwLock<Option<SecuritySettings>>,
    /// Records shed because the queue was full or already closed.
    dropped: AtomicU64,
    /// Records the writer could not save: a batch that failed twice, or a
    /// single record the database refused (OBS-02).
    lost: AtomicU64,
    /// Batches whose first save failed and were tried again.
    retried: AtomicU64,
    /// The record losses the last "records not saved" alert covered
    /// (`services::record_loss`).
    pub(crate) loss_alerted: AtomicU64,
    /// The writer task, owned here so shutdown can wait for its last save.
    writer: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Monitor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(CHANNEL_CAP);
        Self {
            tx,
            rx: Mutex::new(Some(rx)),
            bans: RwLock::new(BanCache::default()),
            settings: RwLock::new(None),
            dropped: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            retried: AtomicU64::new(0),
            loss_alerted: AtomicU64::new(0),
            writer: Mutex::new(None),
        }
    }

    pub fn send(&self, r: Record) {
        if self.tx.try_send(r).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Records the writer could not save, since start.
    pub fn lost(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }

    /// Batches saved only on their second try, or not at all, since start.
    pub fn retried(&self) -> u64 {
        self.retried.load(Ordering::Relaxed)
    }

    /// Whether `ip`, in any spelling, is banned. Bans are held and looked up
    /// in the one canonical spelling (`crate::server::addr`), so
    /// `::ffff:1.2.3.4` is refused when `1.2.3.4` is banned.
    pub fn is_banned(&self, ip: &str, now: DateTime<Utc>) -> bool {
        let Some(ip) = crate::server::addr::canonical_text(ip) else {
            return false;
        };
        if store::never_banned(&ip) {
            return false;
        }
        match self.bans.read().bans.get(&ip) {
            Some(None) => true,
            Some(Some(exp)) => *exp > now,
            None => false,
        }
    }

    /// Whether a device on the network is banned: its own address, or the
    /// key it is counted and automatically banned under (a foreign IPv6
    /// device's /64; `crate::server::addr::budget_key`).
    pub fn is_banned_peer(&self, ip: std::net::IpAddr, now: DateTime<Utc>) -> bool {
        let key = crate::server::addr::budget_key(ip);
        self.is_banned(&ip.to_string(), now)
            || (key != crate::server::addr::canonical(ip) && self.is_banned(&key.to_string(), now))
    }

    pub fn ban_count(&self) -> usize {
        self.bans.read().bans.len()
    }

    /// Reload the ban list from `logs.db`.
    pub fn reload_bans(&self, ctx: &AppState) {
        let now = ctx.now();
        let loaded = ctx.logs.conn().and_then(|c| store::all_bans(&c, now));
        match loaded {
            Ok(rows) => {
                let bans = rows
                    .into_iter()
                    .filter_map(|b| {
                        let exp = if b.is_permanent {
                            None
                        } else {
                            b.expires_at.as_deref().and_then(store::parse_ts)
                        };
                        crate::server::addr::canonical_text(&b.ip_address).map(|ip| (ip, exp))
                    })
                    .collect();
                self.bans.write().bans = bans;
            }
            Err(e) => tracing::warn!("Could not load the blocked address list: {}", e),
        }
    }

    pub fn security_settings(&self, ctx: &AppState) -> SecuritySettings {
        if let Some(s) = *self.settings.read() {
            return s;
        }
        let s = ctx
            .sqlite
            .conn()
            .and_then(|c| webui::security_settings(&c))
            .unwrap_or_default();
        *self.settings.write() = Some(s);
        s
    }

    pub fn invalidate_settings(&self) {
        *self.settings.write() = None;
    }

    /// Persist whatever is queued, synchronously. Used by tests and by the
    /// writer when it starts; a running writer owns the receiver, and then
    /// this does nothing.
    pub fn drain_now(&self, ctx: &AppState) -> usize {
        let mut guard = self.rx.lock();
        let Some(rx) = guard.as_mut() else {
            return 0;
        };
        let mut batch = Vec::new();
        while let Ok(r) = rx.try_recv() {
            batch.push(r);
        }
        drop(guard);
        let n = batch.len();
        save_batch(ctx, batch);
        n
    }

    /// Wait up to `grace` for the writer's last save after shutdown was
    /// signalled, then stop it.
    pub async fn stop(&self, grace: Duration) {
        let Some(mut writer) = self.writer.lock().take() else {
            return;
        };
        if tokio::time::timeout(grace, &mut writer).await.is_err() {
            tracing::warn!("Monitoring records were still being saved at exit");
            writer.abort();
        }
    }
}

/// Start the writer task (once). It is owned by the monitor; on shutdown it
/// saves what is still queued, then stops (`Monitor::stop`).
pub fn start(ctx: &Arc<AppState>) {
    let Some(rx) = ctx.monitor.rx.lock().take() else {
        return;
    };
    ctx.monitor.reload_bans(ctx);
    let writer = tokio::spawn(write_until_shutdown(ctx.clone(), rx));
    *ctx.monitor.writer.lock() = Some(writer);
}

async fn write_until_shutdown(c: Arc<AppState>, mut rx: mpsc::Receiver<Record>) {
    let token = c.shutdown.child_token();
    let mut housekeeping = tokio::time::interval(HOUSEKEEPING);
    let mut bans = tokio::time::interval(BAN_REFRESH);
    let mut buf = Vec::with_capacity(256);
    loop {
        tokio::select! {
            // Shutdown first, so what is queued then is saved below and not
            // left to whichever branch the select happens to pick.
            biased;
            _ = token.cancelled() => {
                save_rest(&c, rx).await;
                break;
            }
            n = rx.recv_many(&mut buf, 256) => {
                if n == 0 {
                    break;
                }
                let batch = std::mem::take(&mut buf);
                let c2 = c.clone();
                let _ = tokio::task::spawn_blocking(move || save_batch(&c2, batch)).await;
            }
            _ = housekeeping.tick() => {
                let c2 = c.clone();
                let _ = tokio::task::spawn_blocking(move || housekeep(&c2)).await;
            }
            _ = bans.tick() => {
                let c2 = c.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = crate::services::error_log::flush_queue(&c2) {
                        tracing::debug!("Error log flush failed: {}", e);
                    }
                    c2.monitor.reload_bans(&c2);
                })
                .await;
            }
        }
    }
}

/// At shutdown: close the queue (later records are counted as dropped)
/// and save what it still holds, within `FINAL_SAVE` (OBS-02). The queue
/// holds at most `CHANNEL_CAP` records, so this is one bounded write.
async fn save_rest(c: &Arc<AppState>, mut rx: mpsc::Receiver<Record>) {
    rx.close();
    let mut rest = Vec::new();
    while let Ok(r) = rx.try_recv() {
        rest.push(r);
    }
    if rest.is_empty() {
        return;
    }
    let c2 = c.clone();
    let save = tokio::task::spawn_blocking(move || save_batch(&c2, rest));
    if tokio::time::timeout(FINAL_SAVE, save).await.is_err() {
        tracing::warn!("Saving the last monitoring records took too long at exit");
    }
}

/// Purge old rows (bounded tables) and persist queued server errors.
pub fn housekeep(ctx: &AppState) {
    let now = ctx.now();
    if let Err(e) = crate::services::error_log::flush_queue(ctx) {
        tracing::debug!("Error log flush failed: {}", e);
    }
    match ctx.logs.conn().and_then(|c| store::purge(&c, now)) {
        Ok(n) if n > 0 => tracing::debug!("Removed {} old monitoring rows", n),
        Ok(_) => {}
        Err(e) => tracing::warn!("Monitoring cleanup failed: {}", e),
    }
}

/// Save one batch, and once more if the whole batch failed (a busy or
/// unreadable database); what still fails is counted as lost (OBS-02).
fn save_batch(ctx: &AppState, batch: Vec<Record>) {
    if batch.is_empty() {
        return;
    }
    let m = &ctx.monitor;
    let saved = save_with_retry(m, batch.len(), || write_batch(ctx, &batch));
    if saved.is_some_and(|bans_changed| bans_changed) {
        m.reload_bans(ctx);
    }
}

/// Run `write` (one batch of `n` records); a failure is retried once and
/// counted. `Some(bans_changed)` when the batch was saved.
fn save_with_retry(
    m: &Monitor,
    n: usize,
    mut write: impl FnMut() -> crate::error::Result<WriteOutcome>,
) -> Option<bool> {
    let outcome = match write() {
        Ok(o) => o,
        Err(first) => {
            m.retried.fetch_add(1, Ordering::Relaxed);
            tracing::debug!("Saving monitoring records failed, trying again: {}", first);
            match write() {
                Ok(o) => o,
                Err(e) => {
                    m.lost.fetch_add(n as u64, Ordering::Relaxed);
                    tracing::warn!("Could not save {} monitoring records: {}", n, e);
                    return None;
                }
            }
        }
    };
    m.lost.fetch_add(outcome.refused, Ordering::Relaxed);
    Some(outcome.bans_changed)
}

/// What a saved batch did besides saving.
struct WriteOutcome {
    bans_changed: bool,
    /// Records the database refused one by one; the rest were saved.
    refused: u64,
}

/// One batch in one transaction, taken at `BEGIN IMMEDIATE` so a busy
/// database fails the batch as a whole (and it is retried) instead of
/// each record waiting out the busy timeout in turn.
fn write_batch(ctx: &AppState, batch: &[Record]) -> crate::error::Result<WriteOutcome> {
    let now = ctx.now();
    let settings = ctx.monitor.security_settings(ctx);
    let mut out = WriteOutcome {
        bans_changed: false,
        refused: 0,
    };
    // This machine's own addresses, read only when a ban is due: an
    // automatic ban never covers one of them.
    let mut own: Option<Vec<std::net::IpAddr>> = None;
    let mut conn = ctx.logs.conn()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for r in batch {
        let step = match r {
            Record::Traffic(t) => store::insert_traffic(&tx, t),
            Record::Latency(l) => store::insert_latency(&tx, l),
            Record::NotFound { ip, path } => store::track_404(&tx, ip, path, now).and_then(|n| {
                if settings.auto_ban_enabled && n >= settings.threshold_404 {
                    out.bans_changed |= store::auto_ban(
                        &tx,
                        ip,
                        &format!("Too many missing pages ({} in a day)", n),
                        hours(settings.ban_duration_404),
                        now,
                        own.get_or_insert_with(crate::server::addr::own_addresses),
                        store::AUTO_BAN_CAP,
                    )?;
                }
                Ok(())
            }),
            Record::InvalidKey { ip } => store::track_invalid_api_key(&tx, ip, now).and_then(|n| {
                if settings.auto_ban_enabled && n >= settings.api_threshold {
                    out.bans_changed |= store::auto_ban(
                        &tx,
                        ip,
                        &format!("Too many invalid API key attempts ({} in a day)", n),
                        hours(settings.api_ban_duration),
                        now,
                        own.get_or_insert_with(crate::server::addr::own_addresses),
                        store::AUTO_BAN_CAP,
                    )?;
                }
                Ok(())
            }),
        };
        if let Err(e) = step {
            out.refused += 1;
            tracing::debug!("Monitoring record skipped: {}", e);
        }
    }
    tx.commit()?;
    Ok(out)
}

/// An automatic ban's duration setting: 0 gives none, so the ban lasts
/// `AUTO_BAN_DEFAULT_HOURS` (`store::auto_ban`). On the web 0 makes it
/// permanent; here an automatic ban always expires (G3), and the Security
/// page offers 0 as "Default (24 hours)".
fn hours(h: i64) -> Option<i64> {
    (h > 0).then_some(h)
}

/// Paths the traffic log skips (web: static files and its own pages).
fn skip_traffic(path: &str) -> bool {
    path.starts_with("/assets/")
        || path.starts_with("/static/")
        || path == "/favicon.ico"
        || path.starts_with("/traffic")
        || path.starts_with("/socket.io")
        || path.starts_with("/api/v1/latency/logs")
}

/// The path as it may be stored: the strategy webhook token and the
/// Chartink webhook id replaced (security review S-06).
pub fn loggable_path(path: &str) -> String {
    store::redact_secret_paths(path)
}

/// Latency record type for an `/api/v1` path (web `api_types`).
pub fn api_type(path: &str) -> String {
    let name = path
        .trim_start_matches("/api/v1/")
        .trim_end_matches('/')
        .to_string();
    let t = match name.as_str() {
        "placeorder" => "PLACE",
        "placesmartorder" => "SMART",
        "modifyorder" => "MODIFY",
        "cancelorder" => "CANCEL",
        "closeposition" => "CLOSE",
        "cancelallorder" => "CANCEL_ALL",
        "basketorder" => "BASKET",
        "splitorder" => "SPLIT",
        "optionsorder" => "OPTIONS",
        "optionsmultiorder" => "OPTIONS_MULTI",
        "placegttorder" => "GTT_PLACE",
        "modifygttorder" => "GTT_MODIFY",
        "cancelgttorder" => "GTT_CANCEL",
        "quotes" => "QUOTES",
        "history" => "HISTORY",
        "depth" => "DEPTH",
        "intervals" => "INTERVALS",
        "funds" => "FUNDS",
        "orderbook" => "ORDERBOOK",
        "tradebook" => "TRADEBOOK",
        "positionbook" => "POSITIONBOOK",
        "holdings" => "HOLDINGS",
        "orderstatus" => "STATUS",
        "openposition" => "POSITION",
        "instruments" => "INSTRUMENTS",
        "search" => "SEARCH",
        "symbol" => "SYMBOL",
        "expiry" => "EXPIRY",
        "margin" => "MARGIN",
        "optiongreeks" => "GREEKS",
        "multioptiongreeks" => "MULTI_GREEKS",
        "optionsymbol" => "OPTION_SYMBOL",
        "syntheticfuture" => "SYNTHETIC",
        "ping" => "PING",
        "analyzer" => "ANALYZER",
        "chart" => "CHART",
        "market/holidays" => "MARKET_HOLIDAYS",
        "market/timings" => "MARKET_TIMINGS",
        n if n.starts_with("ticker") => "TICKER",
        n => return n.split('/').next().unwrap_or(n).to_ascii_uppercase(),
    };
    t.to_string()
}

fn too_large() -> Response {
    crate::server::envelope::error(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large.")
}

/// Orders a trader sends from a page, which the latency log records like
/// `/api/v1` calls (web: the `@track_latency` scalping and Positions
/// routes, OBS-04), with the web's order types.
const PAGE_ORDER_ROUTES: &[(&str, &str)] = &[
    ("/scalping/api/order", "PLACE"),
    ("/scalping/api/close_leg", "CLOSE"),
    ("/scalping/api/close_all", "CLOSE"),
    ("/scalping/api/cancel_all", "CANCEL_ALL"),
    ("/close_position", "CLOSE"),
    ("/close_all_positions", "CLOSE"),
];

/// The latency row type of a request the latency log records, if it does.
fn latency_type(method: &Method, path: &str) -> Option<String> {
    if path.starts_with("/api/v1/") {
        return Some(api_type(path));
    }
    if method != Method::POST {
        return None;
    }
    PAGE_ORDER_ROUTES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, t)| (*t).to_string())
}

/// The monitoring middleware (outermost application layer).
pub async fn layer(State(ctx): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    // The stored caller (`middleware::Source`). Bans apply to a device on
    // the network, by its canonical address; this computer and tunnel
    // callers (whose addresses the app cannot see) are never banned. Every
    // route (API, webhooks, Socket.IO, /mcp, pages) passes this one check;
    // the market data feed asks the same ban list (`feed::auth`).
    let source = crate::server::middleware::source_of(req.extensions());
    let ip = crate::server::addr::canonical(client_ip(&req)).to_string();
    let now = ctx.now();
    let banned = source
        .network_address()
        .is_some_and(|a| ctx.monitor.is_banned_peer(a, now));
    if banned {
        return banned_reply();
    }
    let path = loggable_path(req.uri().path());
    if skip_traffic(&path) {
        return next.run(req).await;
    }
    let method = req.method().clone();
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(String::from);
    let started = Instant::now();

    let resp = match latency_type(&method, &path) {
        Some(order_type) => {
            let call = Timed {
                now,
                path: &path,
                order_type,
            };
            match timed(&ctx, req, next, call).await {
                Ok(r) => r,
                Err(r) => return *r,
            }
        }
        None => next.run(req).await,
    };
    let status = resp.status();
    let elapsed_ms = ms_since(started);
    let (resp, error) = refusal_reason(resp).await;

    if status == StatusCode::FORBIDDEN && path.starts_with("/api/v1/") {
        ctx.monitor.send(Record::InvalidKey { ip: ip.clone() });
    }
    if status == StatusCode::NOT_FOUND {
        ctx.monitor.send(Record::NotFound {
            ip: ip.clone(),
            path: path.clone(),
        });
    }
    ctx.monitor.send(Record::Traffic(TrafficRow {
        timestamp: store::ts(now),
        client_ip: ip,
        method: method.to_string(),
        path,
        status_code: status.as_u16() as i64,
        duration_ms: elapsed_ms,
        host,
        error,
    }));
    resp
}

/// Why a reply of 400 or above refused the request, for the traffic log's
/// Error column: the message of a small JSON reply (`message`, or `error`
/// on the pages' replies; field errors as their JSON text). The web fills
/// the column only when its handler raised an exception, so a refusal there
/// carries no reason and the column is nearly always empty; here it says
/// why, as the latency log does for `/api/v1`.
async fn refusal_reason(resp: Response) -> (Response, Option<String>) {
    if !(resp.status().is_client_error() || resp.status().is_server_error()) {
        return (resp, None);
    }
    let (resp, body) = scan_body(resp).await;
    let said = |k: &str| match body.as_ref()?.get(k)? {
        Value::String(s) => Some(s.clone()),
        fields @ (Value::Object(_) | Value::Array(_)) => Some(fields.to_string()),
        _ => None,
    };
    let reason = said("message").or_else(|| said("error"));
    (resp, reason)
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// The plain-text 403 a banned address gets.
fn banned_reply() -> Response {
    let mut r = (
        StatusCode::FORBIDDEN,
        "Access Denied: Your IP has been banned",
    )
        .into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

/// Read an API POST's body (refused past the body limit) for the symbol it
/// names, and rebuild the request.
async fn with_symbol(req: Request) -> Result<(Request, Option<String>), EarlyReply> {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, crate::config::BODY_LIMIT_BYTES).await {
        Ok(b) => b,
        Err(_) => return Err(Box::new(too_large())),
    };
    let symbol = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| v.get("symbol").and_then(|s| s.as_str()).map(String::from));
    Ok((Request::from_parts(parts, Body::from(bytes)), symbol))
}

/// A request the latency log records.
struct Timed<'a> {
    now: DateTime<Utc>,
    path: &'a str,
    order_type: String,
}

/// Where a recorded request's time went, in ms (web `LatencyTracker`
/// stages): reading the request, the handler, reading the reply.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stages {
    validation_ms: f64,
    handler_ms: f64,
    response_ms: f64,
}

/// A latency row's times, in ms.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Timings {
    rtt_ms: f64,
    overhead_ms: f64,
    total_ms: f64,
}

/// The row's times (web `track_latency`, OBS-03). For an order, `rtt_ms`
/// is the time its broker order calls were in flight, none when nothing
/// reached a broker (the sandbox, a refusal, an order queued for
/// approval), and the overhead is the rest. The web reports the handler's
/// own time, or a quote the sandbox fetched, as broker time then. Any
/// other call keeps the web's fallback: the handler's time as `rtt_ms`,
/// reading the request and the reply as overhead.
fn timings(stages: Stages, broker: &BrokerTime, order: bool) -> Timings {
    let total_ms = stages.validation_ms + stages.handler_ms + stages.response_ms;
    if order {
        let rtt_ms = (broker.busy.as_secs_f64() * 1000.0).min(total_ms);
        return Timings {
            rtt_ms,
            overhead_ms: total_ms - rtt_ms,
            total_ms,
        };
    }
    Timings {
        rtt_ms: stages.handler_ms,
        overhead_ms: stages.validation_ms + stages.response_ms,
        total_ms,
    }
}

/// Run a recorded request: its POST body read for the symbol, the handler
/// under the broker-time probe, the reply read for its order id; then
/// queue its latency row. `Err` is the reply for a body over the limit.
async fn timed(
    ctx: &AppState,
    req: Request,
    next: Next,
    call: Timed<'_>,
) -> Result<Response, EarlyReply> {
    let t = Instant::now();
    let (req, symbol) = if req.method() == Method::POST {
        with_symbol(req).await?
    } else {
        (req, None)
    };
    let validation_ms = ms_since(t);
    let t = Instant::now();
    let (resp, broker) = latency_probe::measure(next.run(req)).await;
    let handler_ms = ms_since(t);
    let t = Instant::now();
    let (resp, body) = scan_body(resp).await;
    let stages = Stages {
        validation_ms,
        handler_ms,
        response_ms: ms_since(t),
    };
    let row = latency_row(
        ctx,
        &call,
        resp.status(),
        body.as_ref(),
        symbol,
        broker.signed_in,
    );
    if let Some(row) = row {
        let order = store::LATENCY_ORDER_TYPES.contains(&row.order_type.as_str());
        let t = timings(stages, &broker, order);
        ctx.monitor.send(Record::Latency(LatencyRow {
            broker: broker.broker.or(row.broker),
            rtt_ms: t.rtt_ms,
            validation_latency_ms: stages.validation_ms,
            response_latency_ms: stages.response_ms,
            overhead_ms: t.overhead_ms,
            total_latency_ms: t.total_ms,
            ..row
        }));
    }
    Ok(resp)
}

/// The row for a recorded reply, without its times; `None` when the web
/// would not have recorded it: a path no API route answers, or a page's
/// order refused before it reached the order (not signed in, a missing
/// CSRF token, the request limit). The broker is the session's, for a call
/// that sent nothing to a broker.
fn latency_row(
    ctx: &AppState,
    call: &Timed<'_>,
    status: StatusCode,
    body: Option<&Value>,
    symbol: Option<String>,
    signed_in: bool,
) -> Option<LatencyRow> {
    let field = |k: &str| {
        body.and_then(|b| b.get(k)).and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    let api = call.path.starts_with("/api/v1/");
    let not_route =
        api && status == StatusCode::NOT_FOUND && body.and_then(|b| b.get("path")).is_some();
    let refused = !api && (!signed_in || status == StatusCode::TOO_MANY_REQUESTS);
    if not_route || refused {
        return None;
    }
    let failed = status.as_u16() >= 400;
    Some(LatencyRow {
        timestamp: store::ts(call.now),
        order_id: field("orderid")
            .or_else(|| field("request_id"))
            .unwrap_or_else(|| "unknown".into()),
        broker: ctx.get_broker_session().map(|b| b.broker_id),
        symbol,
        order_type: call.order_type.clone(),
        status: if failed { "FAILED" } else { "SUCCESS" }.into(),
        error: if failed { field("message") } else { None },
        ..Default::default()
    })
}

/// Read a small JSON response body (rebuilding the response); larger or
/// non-JSON bodies pass through untouched.
async fn scan_body(resp: Response) -> (Response, Option<Value>) {
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("application/json"))
        .unwrap_or(false);
    let small = axum::body::HttpBody::size_hint(resp.body())
        .exact()
        .map(|n| n <= MAX_SCAN_BYTES)
        .unwrap_or(false);
    if !is_json || !small {
        return (resp, None);
    }
    let (parts, body) = resp.into_parts();
    match axum::body::to_bytes(body, MAX_SCAN_BYTES as usize).await {
        Ok(bytes) => {
            let v = serde_json::from_slice::<Value>(&bytes).ok();
            (Response::from_parts(parts, Body::from(bytes)), v)
        }
        Err(_) => (Response::from_parts(parts, Body::from(Bytes::new())), None),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn webhook_secrets_never_reach_the_traffic_log() {
        let token = "oaws_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefg";
        assert_eq!(
            loggable_path(&format!("/strategy/webhook/{}", token)),
            "/strategy/webhook/<redacted>"
        );
        assert_eq!(
            loggable_path("/chartink/webhook/11111111-1111-4111-8111-111111111111"),
            "/chartink/webhook/<redacted>"
        );
        assert_eq!(loggable_path("/api/v1/placeorder"), "/api/v1/placeorder");
        assert_eq!(loggable_path("/chartink/webhook/"), "/chartink/webhook/");
    }

    use super::*;

    fn stages(validation_ms: f64, handler_ms: f64, response_ms: f64) -> Stages {
        Stages {
            validation_ms,
            handler_ms,
            response_ms,
        }
    }

    /// OBS-03: an order's rtt is its broker time and the overhead the rest;
    /// an order that reached no broker has no broker time; other calls keep
    /// the web's fallback.
    #[test]
    fn latency_times_split_broker_time_from_the_platform() {
        let broker = BrokerTime {
            calls: 1,
            busy: Duration::from_millis(100),
            broker: Some("zerodha".into()),
            signed_in: false,
        };
        let t = timings(stages(2.0, 145.0, 3.0), &broker, true);
        assert_eq!((t.rtt_ms, t.overhead_ms, t.total_ms), (100.0, 50.0, 150.0));
        let none = BrokerTime::default();
        let t = timings(stages(2.0, 40.0, 3.0), &none, true);
        assert_eq!((t.rtt_ms, t.overhead_ms, t.total_ms), (0.0, 45.0, 45.0));
        let t = timings(stages(2.0, 40.0, 3.0), &none, false);
        assert_eq!((t.rtt_ms, t.overhead_ms, t.total_ms), (40.0, 5.0, 45.0));
    }

    /// OBS-04: the page order routes the web decorates, and only those.
    #[test]
    fn page_orders_are_recorded_with_the_web_types() {
        let post = |p| latency_type(&Method::POST, p);
        assert_eq!(post("/scalping/api/order").as_deref(), Some("PLACE"));
        assert_eq!(post("/scalping/api/close_leg").as_deref(), Some("CLOSE"));
        assert_eq!(post("/scalping/api/close_all").as_deref(), Some("CLOSE"));
        assert_eq!(
            post("/scalping/api/cancel_all").as_deref(),
            Some("CANCEL_ALL")
        );
        assert_eq!(post("/close_position").as_deref(), Some("CLOSE"));
        assert_eq!(post("/close_all_positions").as_deref(), Some("CLOSE"));
        assert_eq!(post("/cancel_all_orders"), None);
        assert_eq!(latency_type(&Method::GET, "/close_position"), None);
        assert_eq!(post("/api/v1/placeorder").as_deref(), Some("PLACE"));
    }

    #[test]
    fn api_types_follow_the_web_map() {
        assert_eq!(api_type("/api/v1/placeorder"), "PLACE");
        assert_eq!(api_type("/api/v1/market/holidays"), "MARKET_HOLIDAYS");
        assert_eq!(api_type("/api/v1/ticker/NSE:SBIN"), "TICKER");
        assert_eq!(api_type("/api/v1/newthing"), "NEWTHING");
    }

    /// OBS-02: a batch that fails as a whole is tried once more; a second
    /// failure, and each record refused on its own, are counted as lost.
    #[test]
    fn a_failed_batch_is_tried_again_and_what_still_fails_is_counted() {
        let m = Monitor::new();
        let busy = || {
            Err(crate::error::AppError::Internal(
                "database is locked".into(),
            ))
        };
        let saved = |refused| {
            Ok(WriteOutcome {
                bans_changed: true,
                refused,
            })
        };
        let mut calls = 0;
        let r = save_with_retry(&m, 7, || {
            calls += 1;
            if calls == 1 {
                busy()
            } else {
                saved(0)
            }
        });
        assert_eq!((r, calls), (Some(true), 2));
        assert_eq!((m.retried(), m.lost()), (1, 0));

        assert_eq!(save_with_retry(&m, 7, busy), None);
        assert_eq!((m.retried(), m.lost()), (2, 7));

        assert_eq!(save_with_retry(&m, 3, || saved(1)), Some(true));
        assert_eq!((m.retried(), m.lost()), (2, 8));
    }

    #[test]
    fn channel_is_bounded() {
        let m = Monitor::new();
        for _ in 0..(CHANNEL_CAP + 10) {
            m.send(Record::InvalidKey {
                ip: "1.2.3.4".into(),
            });
        }
        assert_eq!(m.dropped(), 10);
    }
}
