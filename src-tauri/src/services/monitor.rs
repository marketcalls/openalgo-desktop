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
//! a query string, or an API key. For `/api/v1` calls the latency recorder
//! reads only `symbol` from the request and `orderid` / `message` from the
//! response.

use crate::db::sqlite::monitor::{self as store, LatencyRow, TrafficRow};
use crate::db::sqlite::webui::{self, SecuritySettings};
use crate::server::middleware::client_ip;
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
    dropped: AtomicU64,
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

    pub fn is_banned(&self, ip: &str, now: DateTime<Utc>) -> bool {
        if store::never_banned(ip) {
            return false;
        }
        match self.bans.read().bans.get(ip) {
            Some(None) => true,
            Some(Some(exp)) => *exp > now,
            None => false,
        }
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
                    .map(|b| {
                        let exp = if b.is_permanent {
                            None
                        } else {
                            b.expires_at.as_deref().and_then(store::parse_ts)
                        };
                        (b.ip_address, exp)
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
        write_batch(ctx, batch);
        n
    }
}

/// Start the writer task (once). It is owned by the context and stops on
/// shutdown.
pub fn start(ctx: &Arc<AppState>) {
    let Some(mut rx) = ctx.monitor.rx.lock().take() else {
        return;
    };
    ctx.monitor.reload_bans(ctx);
    let c = ctx.clone();
    let token = ctx.shutdown.child_token();
    ctx.spawn(async move {
        let mut housekeeping = tokio::time::interval(HOUSEKEEPING);
        let mut bans = tokio::time::interval(BAN_REFRESH);
        let mut buf = Vec::with_capacity(256);
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                n = rx.recv_many(&mut buf, 256) => {
                    if n == 0 {
                        break;
                    }
                    let batch = std::mem::take(&mut buf);
                    let c2 = c.clone();
                    let _ = tokio::task::spawn_blocking(move || write_batch(&c2, batch)).await;
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
    });
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

fn write_batch(ctx: &AppState, batch: Vec<Record>) {
    if batch.is_empty() {
        return;
    }
    let now = ctx.now();
    let settings = ctx.monitor.security_settings(ctx);
    let mut bans_changed = false;
    let res = ctx.logs.conn().and_then(|conn| {
        conn.execute_batch("BEGIN")?;
        for r in batch {
            let step = match r {
                Record::Traffic(t) => store::insert_traffic(&conn, &t),
                Record::Latency(l) => store::insert_latency(&conn, &l),
                Record::NotFound { ip, path } => {
                    store::track_404(&conn, &ip, &path, now).and_then(|n| {
                        if settings.auto_ban_enabled && n >= settings.threshold_404 {
                            bans_changed |= store::ban_ip(
                                &conn,
                                &ip,
                                &format!("Too many missing pages ({} in a day)", n),
                                hours(settings.ban_duration_404),
                                false,
                                "system",
                                now,
                                settings.repeat_offender_limit,
                            )?;
                        }
                        Ok(())
                    })
                }
                Record::InvalidKey { ip } => store::track_invalid_api_key(&conn, &ip, now)
                    .and_then(|n| {
                        if settings.auto_ban_enabled && n >= settings.api_threshold {
                            bans_changed |= store::ban_ip(
                                &conn,
                                &ip,
                                &format!("Too many invalid API key attempts ({} in a day)", n),
                                hours(settings.api_ban_duration),
                                false,
                                "system",
                                now,
                                settings.repeat_offender_limit,
                            )?;
                        }
                        Ok(())
                    }),
            };
            if let Err(e) = step {
                tracing::debug!("Monitoring record skipped: {}", e);
            }
        }
        conn.execute_batch("COMMIT")?;
        Ok(())
    });
    if let Err(e) = res {
        tracing::warn!("Could not save monitoring records: {}", e);
    }
    if bans_changed {
        ctx.monitor.reload_bans(ctx);
    }
}

/// Web convention: 0 hours means permanent.
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

/// The monitoring middleware (outermost application layer).
pub async fn layer(State(ctx): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let ip = client_ip(&req).to_string();
    let now = ctx.now();
    if ctx.monitor.is_banned(&ip, now) {
        let mut r = (
            StatusCode::FORBIDDEN,
            "Access Denied: Your IP has been banned",
        )
            .into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return r;
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
    let is_api = path.starts_with("/api/v1/");
    let started = Instant::now();

    let mut symbol = None;
    let req = if is_api && method == Method::POST {
        let (parts, body) = req.into_parts();
        let bytes = match axum::body::to_bytes(body, crate::config::BODY_LIMIT_BYTES).await {
            Ok(b) => b,
            Err(_) => return too_large(),
        };
        symbol = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("symbol").and_then(|s| s.as_str()).map(String::from));
        Request::from_parts(parts, Body::from(bytes))
    } else {
        req
    };

    let resp = next.run(req).await;
    let status = resp.status();
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

    let resp = if is_api {
        let (resp, body) = scan_body(resp).await;
        let not_route =
            status == StatusCode::NOT_FOUND && body.as_ref().and_then(|b| b.get("path")).is_some();
        if !not_route {
            let field = |k: &str| {
                body.as_ref().and_then(|b| b.get(k)).and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
            };
            let order_id = field("orderid")
                .or_else(|| field("request_id"))
                .unwrap_or_else(|| "unknown".into());
            let error = if status.as_u16() >= 400 {
                field("message")
            } else {
                None
            };
            ctx.monitor.send(Record::Latency(LatencyRow {
                timestamp: store::ts(now),
                order_id,
                broker: ctx.get_broker_session().map(|b| b.broker_id),
                symbol: symbol.take(),
                order_type: api_type(&path),
                rtt_ms: elapsed_ms,
                validation_latency_ms: 0.0,
                response_latency_ms: 0.0,
                overhead_ms: 0.0,
                total_latency_ms: elapsed_ms,
                status: if status.as_u16() < 400 {
                    "SUCCESS".into()
                } else {
                    "FAILED".into()
                },
                error,
                ..Default::default()
            }));
        }
        if status == StatusCode::FORBIDDEN {
            ctx.monitor.send(Record::InvalidKey { ip: ip.clone() });
        }
        resp
    } else {
        resp
    };

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
        error: None,
    }));
    resp
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

    #[test]
    fn api_types_follow_the_web_map() {
        assert_eq!(api_type("/api/v1/placeorder"), "PLACE");
        assert_eq!(api_type("/api/v1/market/holidays"), "MARKET_HOLIDAYS");
        assert_eq!(api_type("/api/v1/ticker/NSE:SBIN"), "TICKER");
        assert_eq!(api_type("/api/v1/newthing"), "NEWTHING");
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
