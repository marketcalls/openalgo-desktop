//! XTS market-data feed (web `streaming/*_websocket.py`, `*_adapter.py`).
//!
//! Transport: Socket.IO (Engine.IO v4 over WebSocket), implemented here on
//! tokio-tungstenite rather than with a Socket.IO crate, on the shared
//! `WebSocketManager`. Before every (re)connect `XtsFeed::prepare` does the
//! XTS work:
//!
//! 1. market-data login `POST {base}{socket_login_path}` with the market
//!    keys when this process knows them (web: on every connect), else the
//!    stored feed token and user id from the broker login;
//! 2. `wss://host{socket_path}/?token&userID&publishFormat=JSON
//!    &broadcastMode=FULL&EIO=4&transport=websocket` (no auth header).
//!
//! The feed speaks Engine.IO through `FeedEvent::Reply`: the Socket.IO
//! connect `40` after the server's open, pongs to server pings (with a
//! heartbeat so a quiet market is not mistaken for a stall); the connect
//! ack accepts the session and `44` refuses it. Subscriptions are REST, not
//! socket frames: `POST {subscription_path}/instruments/subscription`
//! (`PUT` to unsubscribe) with `Authorization: <socket token>`, at most 50
//! instruments per call, 0.5 s apart, run in order by one worker task the
//! feed owns (aborted on reconnect and when the feed is dropped). XTS
//! refuses a whole batch when one instrument in it is already subscribed
//! ("e-session-0002"); for a member with `XtsHooks::split_duplicate_batch`
//! (rmoney) such a batch is retried one instrument at a time so the others
//! still start (web rmoney #2176); for the others the refusal is non-fatal,
//! as in their web adapters. The
//! snapshots in the subscribe answer (`listQuotes`) are decoded like live
//! events with the next frame.
//!
//! `XtsFeed::parse` decodes `NNNN-json-full|partial` events (payload a
//! JSON string or object), `1105` text events and, for jainamxts/rmoney,
//! `xts-binary-packet` attachments. Instruments are matched by
//! `(segment, instrument id)` against the feed's own subscription book, so
//! index tokens (NSE 26000, BSE 1, ...) resolve to their `*_INDEX` rows.
//! No XTS member opens an order-update socket (web uses REST polling).

use super::auth::{market_login_with, MarketSession};
use super::binary;
use super::mapping;
use super::socketio::{self, EioPacket, SioPacket};
use super::{BinaryDecoder, MarketKeys, XtsConfig};
use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedSubscription, Message, NormalizedDepth, NormalizedTick,
    PrepareError, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::Secret;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Instruments per subscription call (web F `adapter.py:36`).
pub const SUBSCRIBE_BATCH: usize = 50;
/// Gap between subscription calls (web F `adapter.py:39`).
pub const SUBSCRIBE_GAP: Duration = Duration::from_millis(500);
/// Pending subscription commands per session.
const COMMAND_QUEUE: usize = 256;
/// Undelivered subscribe snapshots per session (extra ones are dropped;
/// live events follow anyway).
const SNAPSHOT_QUEUE: usize = 512;
/// XTS times are seconds since 1980-01-01 IST.
pub const XTS_EPOCH_OFFSET_SECS: i64 = 315_532_800 - 19_800;

/// Credentials the feed connects with.
#[derive(Clone, Default)]
pub struct FeedSource {
    pub(crate) keys: Option<MarketKeys>,
    pub token: Option<Secret>,
    pub user_id: Option<String>,
}

impl FeedSource {
    /// A source from a stored feed token and user id (no re-login).
    pub fn stored(token: &str, user_id: &str) -> Self {
        Self {
            keys: None,
            token: Some(Secret::new(token)),
            user_id: Some(user_id.to_string()),
        }
    }

    /// A source that logs in with market keys on every connect.
    pub fn with_keys(key: &str, secret: &str) -> Self {
        Self {
            keys: Some(MarketKeys {
                key: Secret::new(key),
                secret: Secret::new(secret),
            }),
            token: None,
            user_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Connect work (before every connect)
// ---------------------------------------------------------------------------

/// Where the feed connects and how it logs in.
struct Connector {
    cfg: &'static XtsConfig,
    http: reqwest::Client,
    base_url: String,
    /// Socket host when it is not the REST host (tests).
    socket_base: Option<String>,
    source: FeedSource,
}
pub fn ws_base(base: &str) -> String {
    if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{}", rest)
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{}", rest)
    } else {
        base.to_string()
    }
}

/// The socket URL (python-socketio: `url + socketio_path + "/?" + query`).
pub fn socket_url(cfg: &XtsConfig, base: &str, token: &str, user_id: &str) -> String {
    let enc = |s: &str| urlencoding::encode(s).into_owned();
    format!(
        "{}{}/?token={}&userID={}&publishFormat=JSON&broadcastMode={}&EIO=4&transport=websocket",
        ws_base(base),
        cfg.socket_path,
        enc(token),
        enc(user_id),
        cfg.broadcast_mode
    )
}

impl Connector {
    async fn session_token(&self) -> std::result::Result<MarketSession, PrepareError> {
        if let Some(keys) = &self.source.keys {
            let url = format!("{}{}", self.base_url, self.cfg.socket_login_path);
            return match market_login_with(
                &self.http,
                self.cfg.id,
                &url,
                keys,
                self.cfg.socket_login_source,
            )
            .await
            {
                Ok(s) if !s.user_id.is_empty() => Ok(s),
                Ok(_) => Err(PrepareError::Unavailable),
                Err(AppError::Auth(m)) => Err(PrepareError::AuthFailed(m)),
                Err(_) => Err(PrepareError::Unavailable),
            };
        }
        match (&self.source.token, &self.source.user_id) {
            (Some(t), Some(u)) if !t.is_empty() && !u.is_empty() => Ok(MarketSession {
                token: t.expose().to_string(),
                user_id: u.clone(),
            }),
            _ => Err(PrepareError::AuthFailed(format!(
                "Live market data needs the {} market data API key. Add it in Profile, Broker Configuration, then log in again.",
                self.cfg.name
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Subscriptions (REST, one worker per connection)
// ---------------------------------------------------------------------------

/// A subscription request queued for the worker.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub subscribe: bool,
    pub code: u16,
    pub instruments: Vec<Value>,
}

/// Whether the market-data token may be sent to `url`.
///
/// Every XTS member is served over TLS (web `broker/*/baseurl.py`: all
/// `https://`), so the `Authorization` token only ever travels encrypted.
/// Plain `http://` is accepted for loopback hosts only, which is what the
/// local test doubles use; any other URL is refused before a byte is sent.
pub fn token_transport_allowed(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    match u.scheme() {
        "https" => true,
        "http" => match u.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            None => false,
        },
        _ => false,
    }
}

/// What one subscription call achieved.
#[derive(Debug, PartialEq, Eq)]
pub enum CallOutcome {
    /// Accepted; the snapshot strings of `listQuotes`.
    Done(Vec<String>),
    /// Refused because an instrument is already subscribed (non-fatal).
    AlreadySubscribed,
}

/// XTS's "Instrument Already Subscribed" refusal (`e-session-0002`), in a
/// JSON description or a plain-text body.
pub fn is_already_subscribed(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("already subscribed") || b.contains("e-session-0002")
}

/// One subscription call.
pub async fn subscription_call(
    http: &reqwest::Client,
    url: &str,
    token: &str,
    subscribe: bool,
    code: u16,
    instruments: &[Value],
) -> Result<CallOutcome> {
    if !token_transport_allowed(url) {
        tracing::error!("XTS subscription refused: market-data host is not served over TLS");
        return Err(AppError::Broker(
            "Live market data could not be started because the broker connection is not secure. Check the broker address in Broker Configuration.".into(),
        ));
    }
    let req = if subscribe {
        http.post(url)
    } else {
        http.put(url)
    };
    let resp = req
        .header("Authorization", token)
        .header("Content-Type", "application/json")
        .json(&json!({"instruments": instruments, "xtsMessageCode": code}))
        .send()
        .await?;
    let body = resp.text().await?;
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if v.get("type").and_then(Value::as_str) != Some("success") {
        if is_already_subscribed(&body) {
            return Ok(CallOutcome::AlreadySubscribed);
        }
        let why = mapping::error_text(&v);
        return Err(AppError::Broker(if why.is_empty() {
            "The broker refused the market data subscription.".into()
        } else {
            why
        }));
    }
    Ok(CallOutcome::Done(
        v.get("result")
            .and_then(|r| r.get("listQuotes"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|q| match q {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    ))
}

/// Where one command's calls go, and how far apart.
pub struct SubscriptionTarget<'a> {
    pub broker: &'static str,
    pub http: &'a reqwest::Client,
    pub url: &'a str,
    pub token: &'a str,
    pub gap: Duration,
    /// `XtsHooks::split_duplicate_batch` of the member.
    pub split_duplicates: bool,
}

/// Run one command: at most `SUBSCRIBE_BATCH` instruments per call, `gap`
/// between calls. With `split_duplicates`, a subscribe batch refused as
/// already subscribed is retried one instrument at a time; without it the
/// refusal is non-fatal. Returns the calls made.
pub async fn run_command(
    t: &SubscriptionTarget<'_>,
    cmd: &Command,
    snapshots: &mpsc::Sender<String>,
    first: &mut bool,
) -> usize {
    let mut calls = 0;
    let mut call = |batch: Vec<Value>| {
        let wait = !*first;
        *first = false;
        calls += 1;
        async move {
            if wait {
                tokio::time::sleep(t.gap).await;
            }
            let r =
                subscription_call(t.http, t.url, t.token, cmd.subscribe, cmd.code, &batch).await;
            (batch, r)
        }
    };
    let mut outcomes = Vec::new();
    for batch in cmd.instruments.chunks(SUBSCRIBE_BATCH) {
        let (batch, r) = call(batch.to_vec()).await;
        match r {
            Ok(CallOutcome::AlreadySubscribed)
                if t.split_duplicates && cmd.subscribe && batch.len() > 1 =>
            {
                tracing::info!(
                    broker = t.broker,
                    code = cmd.code,
                    "Batch holds an instrument already subscribed; subscribing one by one"
                );
                for inst in batch {
                    let (_, r) = call(vec![inst]).await;
                    outcomes.push(r);
                }
            }
            r => outcomes.push(r),
        }
    }
    for r in outcomes {
        match r {
            Ok(CallOutcome::Done(list)) => {
                if cmd.subscribe {
                    for s in list {
                        let _ = snapshots.try_send(s);
                    }
                }
            }
            Ok(CallOutcome::AlreadySubscribed) => {
                tracing::debug!(broker = t.broker, "Instrument already subscribed")
            }
            Err(e) => tracing::warn!(
                broker = t.broker,
                code = cmd.code,
                subscribe = cmd.subscribe,
                "Subscription call failed: {}",
                e
            ),
        }
    }
    calls
}

async fn subscription_worker(
    broker: &'static str,
    split_duplicates: bool,
    http: reqwest::Client,
    url: String,
    token: Secret,
    mut rx: mpsc::Receiver<Command>,
    snapshots: mpsc::Sender<String>,
) {
    let target = SubscriptionTarget {
        broker,
        http: &http,
        url: &url,
        token: token.expose(),
        gap: SUBSCRIBE_GAP,
        split_duplicates,
    };
    let mut first = true;
    while let Some(cmd) = rx.recv().await {
        run_command(&target, &cmd, &snapshots, &mut first).await;
    }
}

// ---------------------------------------------------------------------------
// Feed (manager side)
// ---------------------------------------------------------------------------

pub struct XtsFeed {
    cfg: &'static XtsConfig,
    connector: Connector,
    /// Socket address and token from the last `prepare`.
    socket: Option<(Secret, Secret)>,
    /// `(segment code, instrument id)` -> subscription.
    book: HashMap<(i64, String), FeedSubscription>,
    commands: Option<mpsc::Sender<Command>>,
    worker: Option<JoinHandle<()>>,
    snapshots_tx: mpsc::Sender<String>,
    snapshots_rx: mpsc::Receiver<String>,
    pending_attachments: usize,
    /// When market data last arrived (epoch seconds), for the data-stall
    /// watchdog (`XtsHooks::data_stall_watchdog`).
    last_data: Option<i64>,
}

/// Silence during an open session after which the feed reconnects (web
/// fivepaisaxts `DATA_TIMEOUT`, #2155).
pub const DATA_STALL_SECS: i64 = 90;
/// IST session windows in minutes after midnight, end excluded (web
/// `_EQUITY_SESSION`, `_MCX_SESSION`).
const EQUITY_SESSION: (u32, u32) = (9 * 60 + 15, 15 * 60 + 30);
const MCX_SESSION: (u32, u32) = (9 * 60, 23 * 60 + 30);
/// MCX segment codes: 51 in XTS, 5 in the web's 5paisa constants.
const MCX_SEGMENTS: [i64; 2] = [5, 51];

/// The instant silence is measured from, or `None` when no subscribed
/// segment's session is open (web fivepaisaxts `_stall_reference`): no
/// subscriptions, a weekend in IST, or outside every subscribed segment's
/// window. Otherwise the later of the last market data and the open of the
/// earliest live session, so a socket quiet since before the open is not
/// reconnected the moment trading starts.
pub fn stall_reference(
    now: i64,
    last_data: Option<i64>,
    segments: impl IntoIterator<Item = i64>,
) -> Option<i64> {
    use chrono::{Datelike, TimeZone, Timelike};
    let ist = chrono_tz::Asia::Kolkata.timestamp_opt(now, 0).single()?;
    if ist.weekday().number_from_monday() >= 6 {
        return None;
    }
    let minute = ist.hour() * 60 + ist.minute();
    let midnight = now - i64::from(ist.num_seconds_from_midnight());
    let mut segments = segments.into_iter().peekable();
    segments.peek()?;
    let open = segments
        .filter_map(|seg| {
            let (start, end) = if MCX_SEGMENTS.contains(&seg) {
                MCX_SESSION
            } else {
                EQUITY_SESSION
            };
            (start <= minute && minute < end).then_some(midnight + i64::from(start) * 60)
        })
        .min()?;
    Some(last_data.unwrap_or(0).max(open))
}

/// A Socket.IO market-data event the watchdog counts (web: the handlers
/// wrapped by `_stamped`), whether or not its payload is later used.
fn is_market_event(name: &str) -> bool {
    matches!(
        name.split_once('-'),
        Some((
            "1501" | "1502" | "1505" | "1510" | "1512" | "1105",
            "json-full" | "json-partial"
        ))
    )
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Drop for XtsFeed {
    fn drop(&mut self) {
        if let Some(w) = self.worker.take() {
            w.abort();
        }
    }
}

impl XtsFeed {
    pub fn new(
        cfg: &'static XtsConfig,
        http: reqwest::Client,
        base_url: String,
        socket_base: Option<String>,
        source: FeedSource,
    ) -> Self {
        let (snapshots_tx, snapshots_rx) = mpsc::channel(SNAPSHOT_QUEUE);
        Self {
            cfg,
            connector: Connector {
                cfg,
                http,
                base_url,
                socket_base,
                source,
            },
            socket: None,
            book: HashMap::new(),
            commands: None,
            worker: None,
            snapshots_tx,
            snapshots_rx,
            pending_attachments: 0,
            last_data: None,
        }
    }

    /// Has market data stopped during an open session (as of `now`, epoch
    /// seconds)? Always false for a member without the watchdog.
    pub(crate) fn stalled_at(&self, now: i64) -> bool {
        if !self.cfg.hooks.data_stall_watchdog {
            return false;
        }
        stall_reference(now, self.last_data, self.book.keys().map(|(seg, _)| *seg))
            .is_some_and(|reference| now - reference > DATA_STALL_SECS)
    }

    #[cfg(test)]
    pub(crate) fn set_last_data(&mut self, at: Option<i64>) {
        self.last_data = at;
    }

    #[cfg(test)]
    pub(crate) fn last_data(&self) -> Option<i64> {
        self.last_data
    }

    /// Stop the subscription worker of the previous connection.
    fn stop_worker(&mut self) {
        self.commands = None;
        if let Some(w) = self.worker.take() {
            w.abort();
        }
    }

    fn enqueue(&mut self, cmd: Command) {
        if self.commands.is_none() {
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                tracing::warn!(broker = self.cfg.id, "Subscription needs the async runtime");
                return;
            };
            let token = self
                .socket
                .as_ref()
                .map(|(_, t)| t.clone())
                .or_else(|| self.connector.source.token.clone())
                .unwrap_or_default();
            let (tx, rx) = mpsc::channel(COMMAND_QUEUE);
            self.worker = Some(handle.spawn(subscription_worker(
                self.cfg.id,
                self.cfg.hooks.split_duplicate_batch,
                self.connector.http.clone(),
                format!(
                    "{}{}/instruments/subscription",
                    self.connector.base_url, self.cfg.subscription_path
                ),
                token,
                rx,
                self.snapshots_tx.clone(),
            )));
            self.commands = Some(tx);
        }
        if let Some(tx) = &self.commands {
            if tx.try_send(cmd).is_err() {
                tracing::warn!(
                    broker = self.cfg.id,
                    "Subscription queue full; request dropped"
                );
            }
        }
    }

    /// Subscription snapshots received since the last frame.
    fn drain_snapshots(&mut self) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        while let Ok(s) = self.snapshots_rx.try_recv() {
            out.extend(self.handle_payload(&Value::String(s)));
        }
        out
    }

    fn key(sub: &FeedSubscription) -> Option<(i64, String)> {
        let ex: Exchange = sub.exchange.parse().ok()?;
        Some((mapping::segment_code(ex)?, sub.token.trim().to_string()))
    }

    /// The REST subscription requests for `subs`, one per message code.
    pub fn commands(&self, subs: &[FeedSubscription], subscribe: bool) -> Vec<Command> {
        let mut by_code: Vec<(u16, Vec<Value>)> = Vec::new();
        for s in subs {
            let Some((seg, token)) = Self::key(s) else {
                continue;
            };
            let code = self.cfg.mode_code(s.mode.code());
            let inst = json!({"exchangeSegment": seg, "exchangeInstrumentID": mapping::instrument_id(&token)});
            match by_code.iter_mut().find(|(c, _)| *c == code) {
                Some((_, v)) => v.push(inst),
                None => by_code.push((code, vec![inst])),
            }
        }
        by_code
            .into_iter()
            .map(|(code, instruments)| Command {
                subscribe,
                code,
                instruments,
            })
            .collect()
    }

    /// Decode one market-data message (JSON shape) into events.
    pub fn normalise(&self, v: &Value) -> Vec<FeedEvent> {
        let seg = mapping::i(v, "ExchangeSegment");
        let id = mapping::s(v, "ExchangeInstrumentID");
        let Some(sub) = self.book.get(&(seg, id.trim().to_string())) else {
            return Vec::new();
        };
        normalise_message(v, sub)
    }

    fn handle_payload(&self, payload: &Value) -> Vec<FeedEvent> {
        match payload {
            Value::String(s) if s.starts_with("t:") => binary::parse_1105(s)
                .map(|v| self.normalise(&v))
                .unwrap_or_default(),
            Value::String(s) => serde_json::from_str::<Value>(s)
                .map(|v| self.normalise(&v))
                .unwrap_or_default(),
            o @ Value::Object(_) => self.normalise(o),
            _ => Vec::new(),
        }
    }

    /// One Engine.IO / Socket.IO text frame.
    fn parse_text(&mut self, text: &str) -> Vec<FeedEvent> {
        let m = match socketio::decode_eio(text) {
            Some(EioPacket::Open(_)) => {
                return vec![FeedEvent::Reply(Message::Text(socketio::CONNECT.into()))]
            }
            Some(EioPacket::Ping(p)) => {
                return vec![
                    FeedEvent::Reply(Message::Text(socketio::pong(p))),
                    FeedEvent::Heartbeat,
                ]
            }
            Some(EioPacket::Message(m)) => m,
            _ => return Vec::new(),
        };
        let (name, args) = match socketio::decode_sio(m) {
            Some(SioPacket::Connect(_)) => return vec![FeedEvent::AuthOk],
            Some(SioPacket::ConnectError(v)) => {
                tracing::warn!(
                    broker = self.cfg.id,
                    "Market data socket refused: {}",
                    socketio::error_message(&v)
                );
                return vec![FeedEvent::AuthFailed(format!(
                    "{} refused the live market data session. Log in to {} again.",
                    self.cfg.name, self.cfg.name
                ))];
            }
            Some(SioPacket::BinaryEvent {
                attachments, name, ..
            }) => {
                if name == "xts-binary-packet" {
                    self.pending_attachments += attachments;
                }
                return Vec::new();
            }
            Some(SioPacket::Event { name, args }) => (name, args),
            _ => return Vec::new(),
        };
        if is_market_event(&name) {
            self.last_data = Some(now_secs());
        }
        let wanted = name.ends_with("-json-full")
            || name.ends_with("-json-partial")
            || name == "message"
            || name == "xts-binary-packet";
        if !wanted {
            return Vec::new();
        }
        args.first()
            .map(|a| self.handle_payload(a))
            .unwrap_or_default()
    }

    /// A binary attachment announced by an `xts-binary-packet` event.
    fn parse_binary(&mut self, data: &[u8]) -> Vec<FeedEvent> {
        if self.pending_attachments == 0 {
            return Vec::new();
        }
        self.pending_attachments -= 1;
        let data = socketio::strip_eio3_prefix(data);
        let decoded = match self.cfg.binary_decoder {
            Some(BinaryDecoder::Jainam) => binary::decode_jainam(data),
            Some(BinaryDecoder::Rmoney) => binary::decode_rmoney(data),
            None => None,
        };
        decoded.map(|v| self.normalise(&v)).unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn insert(&mut self, sub: FeedSubscription) {
        if let Some(k) = Self::key(&sub) {
            self.book.insert(k, sub);
        }
    }
}

fn mode_for_code(code: i64) -> Option<u8> {
    match code {
        1512 => Some(1),
        1501 => Some(2),
        1502 => Some(3),
        _ => None,
    }
}

/// XTS `LastTradedTime` (seconds since 1980-01-01 IST) -> epoch ms.
pub fn xts_time_ms(t: i64) -> i64 {
    if t <= 0 {
        0
    } else {
        (t + XTS_EPOCH_OFFSET_SECS) * 1000
    }
}

fn depth_side(v: Option<&Value>) -> Vec<DepthLevel> {
    let mut out: Vec<DepthLevel> = v
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|l| l.is_object())
                .take(20)
                .map(|l| DepthLevel {
                    price: mapping::f(l, "Price"),
                    quantity: mapping::i(l, "Size"),
                    orders: mapping::i(l, "TotalOrders"),
                })
                .collect()
        })
        .unwrap_or_default();
    if out.len() < 5 {
        out.resize(5, DepthLevel::default());
    }
    out
}

/// web `_normalize_market_data` (`adapter.py:970-1104`). A message without
/// a `MessageCode` (1105 text) is a touchline.
pub fn normalise_message(v: &Value, sub: &FeedSubscription) -> Vec<FeedEvent> {
    let mode = match v.get("MessageCode") {
        None => 2,
        Some(_) => match mode_for_code(mapping::i(v, "MessageCode")) {
            Some(m) => m,
            None => return Vec::new(),
        },
    };
    let t = v.get("Touchline").filter(|t| t.is_object()).unwrap_or(v);
    let ltq = mapping::num(
        t.get("LastTradedQunatity")
            .or_else(|| t.get("LastTradedQuantity")),
    ) as i64;
    let avg = mapping::num(
        t.get("AverageTradedPrice")
            .or_else(|| t.get("AveragePrice")),
    );
    let now = now_ms();
    let mut tick = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode,
        ltp: mapping::f(t, "LastTradedPrice"),
        last_quantity: ltq,
        last_trade_time_ms: xts_time_ms(mapping::i(t, "LastTradedTime")),
        timestamp_ms: now,
        ..Default::default()
    };
    if mode >= 2 {
        tick.open = mapping::f(t, "Open");
        tick.high = mapping::f(t, "High");
        tick.low = mapping::f(t, "Low");
        tick.close = mapping::f(t, "Close");
        tick.volume = mapping::i(t, "TotalTradedQuantity");
        tick.average_price = avg;
        tick.total_buy_quantity = mapping::i(t, "TotalBuyQuantity");
        tick.total_sell_quantity = mapping::i(t, "TotalSellQuantity");
        tick.derive_change();
    }
    if mode == 3 {
        tick.oi = mapping::i(v, "OpenInterest");
    }
    let mut out = Vec::with_capacity(2);
    let depth = (mode == 3 && v.get("Bids").is_some() && v.get("Asks").is_some()).then(|| {
        NormalizedDepth {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            ltp: tick.ltp,
            buy: depth_side(v.get("Bids")),
            sell: depth_side(v.get("Asks")),
            total_buy_quantity: tick.total_buy_quantity,
            total_sell_quantity: tick.total_sell_quantity,
            timestamp_ms: now,
        }
    });
    out.push(FeedEvent::Tick(tick));
    if let Some(d) = depth {
        out.push(FeedEvent::Depth(d));
    }
    out
}

#[async_trait]
impl BrokerFeed for XtsFeed {
    fn broker(&self) -> &'static str {
        self.cfg.id
    }

    /// Market-data login (web: on every connect) and the socket address.
    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        self.stop_worker();
        let session = self.connector.session_token().await?;
        let base = self
            .connector
            .socket_base
            .as_deref()
            .unwrap_or(&self.connector.base_url);
        let url = socket_url(self.cfg, base, &session.token, &session.user_id);
        self.socket = Some((Secret::new(url), Secret::new(session.token)));
        Ok(())
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let invalid = || {
            tracing::error!(
                broker = self.cfg.id,
                "Market data socket address is invalid"
            );
            AppError::Broker(format!(
                "{} live market data could not be started. Log in to {} again.",
                self.cfg.name, self.cfg.name
            ))
        };
        let (url, _) = self.socket.as_ref().ok_or_else(invalid)?;
        url.expose().into_client_request().map_err(|_| invalid())
    }

    /// XTS answers a stale token with 400 as well as 401 / 403.
    fn is_auth_failure(&self, http_status: Option<u16>) -> bool {
        matches!(http_status, Some(400) | Some(401) | Some(403))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.pending_attachments = 0;
        while self.snapshots_rx.try_recv().is_ok() {}
        // Web `_on_connect`: silence is measured from the connection.
        self.last_data = Some(now_secs());
        Vec::new()
    }

    fn data_stalled(&mut self) -> bool {
        let now = now_secs();
        if !self.stalled_at(now) {
            return false;
        }
        tracing::warn!(
            broker = self.cfg.id,
            "No market data for over {} s during an open session; reconnecting",
            DATA_STALL_SECS
        );
        true
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        for s in subs {
            if let Some(k) = Self::key(s) {
                self.book.insert(k, s.clone());
            }
        }
        for c in self.commands(subs, true) {
            self.enqueue(c);
        }
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        for s in subs {
            if let Some(k) = Self::key(s) {
                self.book.remove(&k);
            }
        }
        for c in self.commands(subs, false) {
            self.enqueue(c);
        }
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        let mut out = self.drain_snapshots();
        out.extend(match msg {
            Message::Text(t) => self.parse_text(t),
            Message::Binary(b) => self.parse_binary(b),
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        });
        out
    }
}
