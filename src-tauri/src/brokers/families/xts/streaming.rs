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
//! feed owns (aborted on reconnect and when the feed is dropped). The
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

/// One subscription call. Returns the snapshot strings of `listQuotes`.
pub async fn subscription_call(
    http: &reqwest::Client,
    url: &str,
    token: &str,
    subscribe: bool,
    code: u16,
    instruments: &[Value],
) -> Result<Vec<String>> {
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
    let v: Value = resp.json().await?;
    if v.get("type").and_then(Value::as_str) != Some("success") {
        return Err(AppError::Broker(mapping::error_text(&v)));
    }
    Ok(v.get("result")
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
        .unwrap_or_default())
}

async fn subscription_worker(
    broker: &'static str,
    http: reqwest::Client,
    url: String,
    token: Secret,
    mut rx: mpsc::Receiver<Command>,
    snapshots: mpsc::Sender<String>,
) {
    let mut first = true;
    while let Some(cmd) = rx.recv().await {
        for batch in cmd.instruments.chunks(SUBSCRIBE_BATCH) {
            if !first {
                tokio::time::sleep(SUBSCRIBE_GAP).await;
            }
            first = false;
            match subscription_call(&http, &url, token.expose(), cmd.subscribe, cmd.code, batch)
                .await
            {
                Ok(list) => {
                    if cmd.subscribe {
                        for s in list {
                            let _ = snapshots.try_send(s);
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    broker,
                    code = cmd.code,
                    subscribe = cmd.subscribe,
                    "Subscription call failed: {}",
                    e
                ),
            }
        }
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
        }
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
        Vec::new()
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
