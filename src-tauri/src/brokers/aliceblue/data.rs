//! Quotes, multiquotes, depth and history (web `api/data.py`).
//!
//! AliceBlue has no REST quote API: the web subscribes the instrument on
//! its Noren market socket, waits for the snapshot, reads it and
//! unsubscribes. The same here, over one pooled socket per session
//! (`QuotePool`):
//! * built on first use (invalidate + create the socket session, connect,
//!   log in), replaced when the session token changes or the socket dies;
//! * owned by one task whose handle is aborted when the pool lets go of it;
//!   the task closes the socket when no request has used it for
//!   `QuoteTiming::idle`;
//! * snapshots are kept only for instruments a request is waiting on
//!   (reference counted like the web's `_subscription_refs`) and dropped
//!   with the last reference, so the cache is bounded by the requests in
//!   flight and never by the instruments ever quoted. Frames for any other
//!   instrument are ignored, and at most `MAX_TRACKED` instruments are
//!   tracked at once.
//!
//! History is one call to the chart API (1-minute or daily candles); other
//! intraday intervals are resampled from 1 minute like the web.

use super::mapping::{self, num, s};
use super::streaming::{
    self, auth_ack, connect_frame, heartbeat_frame, subscribe_frame, unsubscribe_frame, DepthSnap,
    QuoteSnap,
};
use super::{broker_error, text, AliceBlueBroker};
use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::streaming::Message;
use crate::brokers::types::*;
use crate::brokers::upstox::relay::{Open, UpstreamWs};
use crate::error::{AppError, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// web multiquote batch size.
pub const BATCH_SIZE: usize = 100;
/// Most instruments one pooled socket tracks at once.
pub const MAX_TRACKED: usize = 2048;
/// Chart API path (web `HISTORICAL_API_URL`).
pub const HISTORY_PATH: &str = "/open-api/od/ChartAPIService/api/chart/history";

// ---------------------------------------------------------------------------
// Pooled quote socket
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ConnState {
    /// `NSE|2885` -> requests waiting on it.
    refs: HashMap<String, u32>,
    quotes: HashMap<String, QuoteSnap>,
    depth: HashMap<String, DepthSnap>,
}

impl ConnState {
    fn apply(&mut self, v: &Value) {
        let key = format!("{}|{}", s(v, "e"), s(v, "tk"));
        if !self.refs.contains_key(&key) {
            return;
        }
        match s(v, "t").as_str() {
            "tk" | "tf" => self.quotes.entry(key).or_default().apply(v),
            "dk" | "df" => self.depth.entry(key).or_default().apply(v),
            _ => {}
        }
    }

    /// Drop one reference per key; returns the keys nobody wants now.
    fn release(&mut self, keys: &[String]) -> Vec<String> {
        let mut gone = Vec::new();
        for k in keys {
            let left = self.refs.get(k).copied().unwrap_or(0).saturating_sub(1);
            if left > 0 {
                self.refs.insert(k.clone(), left);
                continue;
            }
            self.refs.remove(k);
            self.quotes.remove(k);
            self.depth.remove(k);
            gone.push(k.clone());
        }
        gone
    }
}

/// One live, logged-in quote socket.
pub struct Conn {
    session: String,
    tx: mpsc::Sender<String>,
    state: Arc<parking_lot::Mutex<ConnState>>,
    alive: Arc<AtomicBool>,
    last_use: Arc<parking_lot::Mutex<Instant>>,
    task: JoinHandle<()>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Conn {
    fn usable(&self, session: &str) -> bool {
        self.session == session && self.alive.load(Ordering::SeqCst) && !self.task.is_finished()
    }

    /// Instruments currently tracked (tests and diagnostics).
    pub fn tracked(&self) -> usize {
        self.state.lock().refs.len()
    }
}

/// The pooled quote socket of this broker instance.
#[derive(Default)]
pub struct QuotePool {
    slot: parking_lot::Mutex<Option<Arc<Conn>>>,
    build: tokio::sync::Mutex<()>,
}

impl QuotePool {
    /// Close the socket (the task is aborted when the last request using
    /// it finishes).
    pub fn close(&self) {
        self.slot.lock().take();
    }

    /// Whether a live socket is pooled.
    pub fn is_open(&self) -> bool {
        self.slot
            .lock()
            .as_ref()
            .is_some_and(|c| c.alive.load(Ordering::SeqCst) && !c.task.is_finished())
    }

    /// Instruments the pooled socket tracks right now.
    pub fn tracked(&self) -> usize {
        self.slot.lock().as_ref().map(|c| c.tracked()).unwrap_or(0)
    }
}

fn session_id(jwt: &str) -> String {
    super::auth::sha256_hex(jwt)
}

fn market_unavailable() -> AppError {
    AppError::Broker(
        "AliceBlue market data is not reachable right now. Try again in a moment.".into(),
    )
}

async fn acquire(b: &AliceBlueBroker, auth: &AuthToken) -> Result<Arc<Conn>> {
    let session = session_id(auth.raw());
    if let Some(c) = b.quotes.slot.lock().as_ref() {
        if c.usable(&session) {
            return Ok(c.clone());
        }
    }
    // One build at a time; a request that waited takes the socket the
    // first one built (web `_WS_CREATE_LOCK`).
    let _g = b.quotes.build.lock().await;
    if let Some(c) = b.quotes.slot.lock().as_ref() {
        if c.usable(&session) {
            return Ok(c.clone());
        }
    }
    let stale = b.quotes.slot.lock().take();
    drop(stale);
    let ucc = super::auth::ucc(auth).ok_or_else(super::missing_ucc)?;
    let conn = Arc::new(connect(b, auth.raw(), &ucc, session).await?);
    *b.quotes.slot.lock() = Some(conn.clone());
    Ok(conn)
}

async fn connect(b: &AliceBlueBroker, jwt: &str, ucc: &str, session: String) -> Result<Conn> {
    let ws = match streaming::open_market_socket(&b.http, &b.ep, jwt, ucc, b.timing.connect).await {
        Open::Ready(ws) => *ws,
        Open::AuthFailed(m) => return Err(AppError::Auth(m)),
        Open::Unavailable => return Err(market_unavailable()),
    };
    let mut ws = ws;
    let login = async {
        ws.send(Message::Text(connect_frame(jwt, ucc))).await?;
        while let Some(m) = ws.next().await {
            if let Message::Text(t) = m? {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    if let Some(ok) = auth_ack(&v) {
                        return Ok(ok);
                    }
                }
            }
        }
        Ok::<bool, tokio_tungstenite::tungstenite::Error>(false)
    };
    match tokio::time::timeout(b.timing.connect, login).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            let _ = tokio::time::timeout(Duration::from_secs(2), ws.close(None)).await;
            return Err(AppError::Auth(
                "AliceBlue refused the market data session. Log in to AliceBlue again.".into(),
            ));
        }
        _ => {
            let _ = tokio::time::timeout(Duration::from_secs(2), ws.close(None)).await;
            return Err(market_unavailable());
        }
    }
    let (tx, rx) = mpsc::channel::<String>(64);
    let state = Arc::new(parking_lot::Mutex::new(ConnState::default()));
    let alive = Arc::new(AtomicBool::new(true));
    let last_use = Arc::new(parking_lot::Mutex::new(Instant::now()));
    let task = tokio::spawn(run(
        ws,
        rx,
        state.clone(),
        alive.clone(),
        last_use.clone(),
        b.timing.idle,
    ));
    Ok(Conn {
        session,
        tx,
        state,
        alive,
        last_use,
        task,
    })
}

/// The socket task: forwards frames, applies snapshots, keeps the socket
/// alive, and closes it after `idle` without a request.
async fn run(
    ws: UpstreamWs,
    mut rx: mpsc::Receiver<String>,
    state: Arc<parking_lot::Mutex<ConnState>>,
    alive: Arc<AtomicBool>,
    last_use: Arc<parking_lot::Mutex<Instant>>,
    idle: Duration,
) {
    let (mut w, mut r) = ws.split();
    let mut hb =
        tokio::time::interval_at(Instant::now() + streaming::HEARTBEAT, streaming::HEARTBEAT);
    let check = idle
        .min(Duration::from_secs(1))
        .max(Duration::from_millis(10));
    let mut tick = tokio::time::interval(check);
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(frame) => {
                    if w.send(Message::Text(frame)).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            m = r.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        state.lock().apply(&v);
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = hb.tick() => {
                if w.send(Message::Text(heartbeat_frame())).await.is_err() {
                    break;
                }
            }
            _ = tick.tick() => {
                let unused = state.lock().refs.is_empty();
                if unused && last_use.lock().elapsed() >= idle {
                    tracing::debug!("AliceBlue quote socket idle; closing");
                    break;
                }
            }
        }
    }
    alive.store(false, Ordering::SeqCst);
    {
        let mut st = state.lock();
        st.quotes.clear();
        st.depth.clear();
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), w.close()).await;
}

/// Holds references on a set of instruments; releasing (also on drop, so a
/// cancelled request cannot pin them) unsubscribes the ones nobody else
/// wants.
struct Claim {
    conn: Arc<Conn>,
    keys: Vec<String>,
    released: bool,
}

impl Claim {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let gone = self.conn.state.lock().release(&self.keys);
        *self.conn.last_use.lock() = Instant::now();
        if !gone.is_empty() {
            let _ = self.conn.tx.try_send(unsubscribe_frame(&gone));
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.release();
    }
}

async fn claim(conn: Arc<Conn>, keys: Vec<String>, depth: bool) -> Result<Claim> {
    {
        let mut st = conn.state.lock();
        let new = keys.iter().filter(|k| !st.refs.contains_key(*k)).count();
        if st.refs.len() + new > MAX_TRACKED {
            return Err(AppError::Broker(
                "Too many AliceBlue quotes are being fetched at once. Try again in a moment."
                    .into(),
            ));
        }
        for k in &keys {
            *st.refs.entry(k.clone()).or_insert(0) += 1;
        }
    }
    *conn.last_use.lock() = Instant::now();
    let mut c = Claim {
        conn,
        keys,
        released: false,
    };
    let frame = subscribe_frame(&c.keys, depth);
    if c.conn.tx.send(frame).await.is_err() {
        c.release();
        return Err(market_unavailable());
    }
    Ok(c)
}

/// Poll until `ready` holds or the deadline passes.
async fn wait_until(poll: Duration, within: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(poll).await;
    }
}

// ---------------------------------------------------------------------------
// Instruments
// ---------------------------------------------------------------------------

/// Socket key for a quote request (web `_map_exchange` + `_normalize_token`).
pub fn quote_key(exchange: &str, token: &str) -> String {
    format!(
        "{}|{}",
        streaming::ab_exchange(exchange),
        mapping::normalize_token(token)
    )
}

fn resolve(b: &AliceBlueBroker, key: &QuoteKey) -> Result<String> {
    let row = b
        .symbols
        .by_symbol(&key.exchange, &key.symbol)
        .filter(|r| !r.token.is_empty())
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })?;
    Ok(quote_key(&key.exchange, &row.token))
}

/// web quote dict -> `Quote` (`close` is the previous close).
pub fn quote_from_snap(key: &QuoteKey, q: &QuoteSnap) -> Quote {
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: q.ltp,
        open: q.open,
        high: q.high,
        low: q.low,
        close: q.close,
        volume: q.volume,
        bid: q.bid,
        ask: q.ask,
        bid_qty: q.bid_qty,
        ask_qty: q.ask_qty,
        oi: q.oi,
        change: 0.0,
        change_percent: 0.0,
        timestamp: String::new(),
    }
}

/// web `get_depth` shape: exactly five levels a side.
pub fn depth_from_snap(key: &QuoteKey, d: &DepthSnap) -> MarketDepth {
    let pad = |levels: &[DepthLevel]| -> Vec<DepthLevel> {
        (0..5)
            .map(|i| {
                levels
                    .get(i)
                    .map(|l| DepthLevel {
                        price: l.price,
                        quantity: l.quantity,
                        orders: 0,
                    })
                    .unwrap_or_default()
            })
            .collect()
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad(&d.bids),
        asks: pad(&d.asks),
        ltp: d.ltp,
        ltq: d.ltq,
        open: d.open,
        high: d.high,
        low: d.low,
        prev_close: d.close,
        volume: d.volume,
        oi: d.oi,
        total_buy_qty: d.total_buy_qty,
        total_sell_qty: d.total_sell_qty,
    }
}

async fn try_quote(b: &AliceBlueBroker, auth: &AuthToken, k: &str) -> Result<Option<QuoteSnap>> {
    let conn = acquire(b, auth).await?;
    let mut c = claim(conn.clone(), vec![k.to_string()], false).await?;
    let st = conn.state.clone();
    let key = k.to_string();
    wait_until(b.timing.poll, b.timing.single, || {
        st.lock().quotes.get(&key).is_some_and(|q| q.full)
    })
    .await;
    let snap = st.lock().quotes.get(k).cloned();
    c.release();
    Ok(snap)
}

pub async fn get_quote(b: &AliceBlueBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let k = resolve(b, key)?;
    let mut last_err = None;
    for attempt in 0..2 {
        match try_quote(b, auth, &k).await {
            Ok(Some(q)) => return Ok(quote_from_snap(key, &q)),
            Ok(None) => {}
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => last_err = Some(e),
        }
        if attempt == 0 {
            // Retry on a fresh socket (web `get_websocket(force_new=True)`).
            b.quotes.close();
            tokio::time::sleep(b.timing.retry_pause).await;
        }
    }
    if let Some(e) = last_err {
        tracing::warn!("AliceBlue quote failed: {}", e.code());
    }
    Err(AppError::Broker(format!(
        "AliceBlue sent no price for {} on {}. The market may be closed or the instrument inactive; try again.",
        key.symbol, key.exchange
    )))
}

fn multi_deadline(b: &AliceBlueBroker, n: usize) -> Duration {
    let t = &b.timing;
    (t.multi_per_symbol * n as u32)
        .max(t.multi_floor)
        .min(t.multi_ceiling)
}

pub async fn get_multiquotes(
    b: &AliceBlueBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let mut out = Vec::with_capacity(keys.len());
    for batch in keys.chunks(BATCH_SIZE) {
        out.extend(multiquote_batch(b, auth, batch).await?);
    }
    Ok(out)
}

fn result(key: &QuoteKey, data: Option<Quote>, error: Option<&str>) -> QuoteResult {
    QuoteResult {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        data,
        error: error.map(str::to_string),
    }
}

async fn multiquote_batch(
    b: &AliceBlueBroker,
    auth: &AuthToken,
    batch: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let resolved: Vec<Option<String>> = batch.iter().map(|k| resolve(b, k).ok()).collect();
    let mut wanted: Vec<String> = Vec::new();
    for k in resolved.iter().flatten() {
        if !wanted.contains(k) {
            wanted.push(k.clone());
        }
    }
    if wanted.is_empty() {
        return Ok(batch
            .iter()
            .map(|k| result(k, None, Some("Could not resolve token")))
            .collect());
    }
    let conn = acquire(b, auth).await?;
    let claimed = match claim(conn.clone(), wanted.clone(), false).await {
        Ok(c) => Some(c),
        Err(_) => {
            // Retry once on a fresh socket (web).
            b.quotes.close();
            let conn = acquire(b, auth).await?;
            claim(conn, wanted.clone(), false).await.ok()
        }
    };
    let Some(mut claimed) = claimed else {
        return Ok(batch
            .iter()
            .zip(&resolved)
            .map(|(k, r)| match r {
                Some(_) => result(k, None, Some("Subscription failed")),
                None => result(k, None, Some("Could not resolve token")),
            })
            .collect());
    };
    let st = claimed.conn.state.clone();
    let all_in = |keys: &[String]| {
        let g = st.lock();
        keys.iter().all(|k| g.quotes.get(k).is_some_and(|q| q.full))
    };
    let first = wait_until(b.timing.poll, multi_deadline(b, wanted.len()), || {
        all_in(&wanted)
    })
    .await;
    if !first {
        wait_until(b.timing.poll, b.timing.straggler, || all_in(&wanted)).await;
    }
    let snaps: HashMap<String, QuoteSnap> = {
        let g = st.lock();
        wanted
            .iter()
            .filter_map(|k| {
                g.quotes
                    .get(k)
                    .filter(|q| q.full)
                    .map(|q| (k.clone(), q.clone()))
            })
            .collect()
    };
    claimed.release();
    let received = snaps.len();
    tracing::debug!(
        "AliceBlue quotes for {}/{} instruments",
        received,
        wanted.len()
    );
    Ok(batch
        .iter()
        .zip(&resolved)
        .map(|(k, r)| match r {
            None => result(k, None, Some("Could not resolve token")),
            Some(sk) => match snaps.get(sk) {
                Some(q) => result(k, Some(quote_from_snap(k, q)), None),
                None => result(k, None, Some("No data received")),
            },
        })
        .collect())
}

pub async fn get_market_depth(
    b: &AliceBlueBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let k = resolve(b, key)?;
    let conn = acquire(b, auth).await?;
    let mut c = claim(conn.clone(), vec![k.clone()], true).await?;
    let st = conn.state.clone();
    wait_until(b.timing.poll, b.timing.single, || {
        st.lock().depth.contains_key(&k)
    })
    .await;
    let snap = st.lock().depth.get(&k).cloned();
    c.release();
    match snap {
        Some(d) => Ok(depth_from_snap(key, &d)),
        None => Err(AppError::Broker(format!(
            "AliceBlue sent no market depth for {} on {}. The market may be closed; try again.",
            key.symbol, key.exchange
        ))),
    }
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Minutes of the intervals resampled from 1-minute candles.
pub fn resample_minutes(interval: &str) -> Option<i64> {
    match interval {
        "3m" => Some(3),
        "5m" => Some(5),
        "10m" => Some(10),
        "15m" => Some(15),
        "30m" => Some(30),
        "1h" => Some(60),
        _ => None,
    }
}

/// Chart API exchange: indices need the `::index` suffix (web).
pub fn history_exchange(exchange: &str) -> String {
    match exchange {
        "NSE_INDEX" => "NSE::index".into(),
        "BSE_INDEX" => "BSE::index".into(),
        "MCX_INDEX" => "MCX::index".into(),
        other => other.into(),
    }
}

const DAY_MS: i64 = 86_400_000;

fn ist_ms(date: NaiveDate, time: NaiveTime) -> i64 {
    mapping::ist()
        .from_local_datetime(&date.and_time(time))
        .single()
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// `(from, to)` in epoch ms per the web's `convert_to_unix_ms` and the
/// adjustments after it; `None` when the start is in the future.
pub fn history_window(
    start: NaiveDate,
    end: NaiveDate,
    daily: bool,
    now_ms: i64,
) -> Option<(i64, i64)> {
    let t = |h, m, s| NaiveTime::from_hms_opt(h, m, s).unwrap_or(NaiveTime::MIN);
    let from = if daily {
        ist_ms(start, t(0, 0, 0))
    } else {
        ist_ms(start, t(9, 15, 0))
    };
    let mut to = ist_ms(end, t(23, 59, 59));
    if from > now_ms {
        return None;
    }
    if to > now_ms {
        to = now_ms;
    }
    if daily {
        // Daily needs a (UTC, as pandas normalises it) day boundary.
        let boundary = to.div_euclid(DAY_MS) * DAY_MS;
        if boundary != to {
            to = boundary + DAY_MS;
        }
    }
    if from == to {
        to += DAY_MS;
    }
    if !daily && to - from < 3_600_000 {
        to = from + 3_600_000;
    }
    Some((from, to))
}

/// A chart `time` (`YYYY-MM-DD HH:MM:SS` IST, or epoch s/ms) as a naive
/// IST date-time.
pub fn parse_time(v: &Value) -> Option<NaiveDateTime> {
    match v {
        Value::Number(n) => {
            let x = n.as_f64()? as i64;
            let ms = if x > 100_000_000_000 { x } else { x * 1000 };
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|d| d.with_timezone(&mapping::ist()).naive_local())
        }
        Value::String(s) => {
            let s = s.trim();
            for f in [
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%dT%H:%M:%S",
                "%Y-%m-%d %H:%M",
                "%d-%m-%Y %H:%M:%S",
            ] {
                if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
                    return Some(d);
                }
            }
            if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                return Some(d.and_time(NaiveTime::MIN));
            }
            s.parse::<f64>().ok().and_then(|x| parse_time(&json!(x)))
        }
        _ => None,
    }
}

/// Chart rows -> candles: daily bars at IST midnight, intraday bars
/// floored to the minute (AliceBlue stamps `HH:MM:59`), sorted and
/// de-duplicated, OI 0 (the chart API carries none).
pub fn candles_from(rows: &[Value], daily: bool) -> Vec<Candle> {
    let tz = mapping::ist();
    let mut out: Vec<Candle> = rows
        .iter()
        .filter_map(|r| {
            let dt = parse_time(r.get("time")?)?;
            let local = if daily {
                dt.date().and_time(NaiveTime::MIN)
            } else {
                dt.date().and_time(
                    NaiveTime::from_hms_opt(
                        chrono::Timelike::hour(&dt),
                        chrono::Timelike::minute(&dt),
                        0,
                    )
                    .unwrap_or(NaiveTime::MIN),
                )
            };
            let ts = tz.from_local_datetime(&local).single()?.timestamp();
            Some(Candle {
                timestamp: ts,
                open: num(r.get("open")),
                high: num(r.get("high")),
                low: num(r.get("low")),
                close: num(r.get("close")),
                volume: num(r.get("volume")) as i64,
                oi: 0,
            })
        })
        .collect();
    out = crate::brokers::common::history::sort_dedupe(out);
    out
}

/// pandas `resample(f"{m}min", label="left", closed="left")` in IST:
/// buckets aligned to IST midnight, first open, max high, min low, last
/// close, summed volume, last OI; empty buckets dropped.
pub fn resample(candles: &[Candle], minutes: i64) -> Vec<Candle> {
    let width = minutes * 60;
    let mut out: Vec<Candle> = Vec::new();
    for c in candles {
        let bucket = c.timestamp - (c.timestamp + 19_800).rem_euclid(width);
        match out.last_mut() {
            Some(last) if last.timestamp == bucket => {
                last.high = last.high.max(c.high);
                last.low = last.low.min(c.low);
                last.close = c.close;
                last.volume += c.volume;
                last.oi = c.oi;
            }
            _ => out.push(Candle {
                timestamp: bucket,
                ..*c
            }),
        }
    }
    out
}

pub async fn get_history(
    b: &AliceBlueBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let resolution = super::TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == req.interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Interval {} is not available for AliceBlue. Use one of 1m, 3m, 5m, 10m, 15m, 30m, 1h, D.",
                req.interval
            ))
        })?;
    let exchange: Exchange = req.key.exchange.parse().map_err(
        |e: crate::brokers::common::mapping::InvalidConstant| AppError::Validation(e.to_string()),
    )?;
    if exchange == Exchange::Bcd {
        tracing::warn!("AliceBlue has no history for BCD");
        return Ok(Vec::new());
    }
    let row = b
        .symbols
        .by_symbol(&req.key.exchange, &req.key.symbol)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                req.key.symbol, req.key.exchange
            ))
        })?;
    let daily = resolution == "D";
    let Some((from, to)) = history_window(
        req.start,
        req.end,
        daily,
        chrono::Utc::now().timestamp_millis(),
    ) else {
        tracing::warn!("AliceBlue history start date is in the future");
        return Ok(Vec::new());
    };
    let body = json!({
        "token": mapping::normalize_token(&row.token),
        "exchange": history_exchange(&req.key.exchange),
        "from": from.to_string(),
        "to": to.to_string(),
        "resolution": resolution,
    });
    let (_, v) = b
        .call(Method::POST, HISTORY_PATH, auth, Some(&body), true)
        .await?;
    let stat = text(v.get("stat")).to_ascii_lowercase();
    let rows = match v.get("result") {
        Some(Value::Array(rows)) if stat != "not_ok" && stat != "not ok" => rows,
        _ => {
            let emsg = text(v.get("emsg"));
            if emsg.to_ascii_lowercase().contains("session") && !emsg.contains("market") {
                return Err(broker_error(&v, "AliceBlue refused the history request."));
            }
            tracing::info!("AliceBlue history has no data: {}", emsg);
            return Ok(Vec::new());
        }
    };
    let candles = candles_from(rows, daily);
    Ok(match resample_minutes(&req.interval) {
        Some(m) => resample(&candles, m),
        None => candles,
    })
}
