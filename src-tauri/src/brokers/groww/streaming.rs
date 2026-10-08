//! Groww live data: NATS over WebSocket with protobuf payloads (web
//! `streaming/nats_websocket.py`, `groww_nats.py`, `groww_adapter.py`).
//!
//! `GrowwFeed` runs on the shared manager. Before every (re)connect its
//! `prepare` does the Groww-specific work:
//! 1. a fresh Ed25519 nkey pair; `POST /v1/api/apex/v1/socket/token/create/`
//!    with `{"socketKey": "<U...>"}` -> `{token, subscriptionId}` (on any
//!    failure the web falls back to the auth token and `direct_auth`);
//! 2. `wss://socket-api.groww.in` with the web's headers;
//! 3. NATS: server `INFO` (nonce) -> `CONNECT {jwt, nkey, sig}` + `PING`;
//!    `+OK` / the first `PONG` (or 2 s, like the web) means ready; server
//!    `PING` gets `PONG`; the client pings every 10 s; `-ERR` naming
//!    authorization ends the session as an auth failure.
//!
//! `parse` buffers frames into NATS ops (an op may span frames, a frame may
//! hold several); protocol replies go back as `FeedEvent::Reply`, and the
//! manager treats the session as accepted after 2 s without `+OK`.
//! Subjects: `/ld/{eq|fo}/{nse|bse}/price.{token}` (LTP/quote) and
//! `.../book.{token}` (depth). NSE indices use the OpenAlgo symbol as the
//! token, BSE indices the numeric token; depth on an index falls back to
//! price. Depth subscriptions add a shadow price subscription because the
//! book carries no LTP/OHLC, and a per-instrument merge cache joins them.

use super::nkeys::KeyPair;
use super::proto;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, PrepareError, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::Secret;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub const SOCKET_TOKEN_URL: &str = "https://api.groww.in/v1/api/apex/v1/socket/token/create/";
pub const WS_URL: &str = "wss://socket-api.groww.in";
/// Client NATS keepalive (web: PING every 10 s).
pub const NATS_PING_EVERY: Duration = Duration::from_secs(10);
/// Treat the session as authenticated after this long without `+OK`.
pub const ASSUME_READY_AFTER: Duration = Duration::from_secs(2);
/// Socket-token request budget (web: 15 s).
const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest buffered partial NATS op; anything bigger is a broken stream.
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

/// Where the feed connects (overridable for tests).
#[derive(Debug, Clone)]
pub struct FeedEndpoints {
    pub socket_token_url: String,
    pub ws_url: String,
}

impl Default for FeedEndpoints {
    fn default() -> Self {
        Self {
            socket_token_url: SOCKET_TOKEN_URL.into(),
            ws_url: WS_URL.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// NATS protocol
// ---------------------------------------------------------------------------

/// One NATS server operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Info(Value),
    Msg {
        subject: String,
        sid: u64,
        payload: Vec<u8>,
    },
    Ping,
    Pong,
    Ok,
    Err(String),
    Other(String),
}

fn find_crlf(b: &[u8]) -> Option<usize> {
    b.windows(2).position(|w| w == b"\r\n")
}

/// Parse the first complete op in `buf`: `Some((op, bytes consumed))`, or
/// `None` when more bytes are needed.
pub fn next_op(buf: &[u8]) -> Option<(Op, usize)> {
    let end = find_crlf(buf)?;
    let line = String::from_utf8_lossy(&buf[..end]).to_string();
    let after = end + 2;
    let mut words = line.split_whitespace();
    let verb = words.next().unwrap_or("").to_ascii_uppercase();
    match verb.as_str() {
        "MSG" | "HMSG" => {
            let args: Vec<&str> = words.collect();
            let headers = verb == "HMSG";
            // MSG subj sid [reply] len ; HMSG subj sid [reply] hlen len
            let (subject, sid, hlen, len) = match (headers, args.len()) {
                (false, 3) => (args[0], args[1], 0, args[2]),
                (false, 4) => (args[0], args[1], 0, args[3]),
                (true, 4) => (args[0], args[1], args[2].parse().ok()?, args[3]),
                (true, 5) => (args[0], args[1], args[3].parse().ok()?, args[4]),
                _ => return Some((Op::Other(line), after)),
            };
            let len: usize = len.parse().ok()?;
            if buf.len() < after + len + 2 {
                return None;
            }
            let body = &buf[after..after + len];
            let payload = body.get(hlen.min(len)..).unwrap_or(&[]).to_vec();
            Some((
                Op::Msg {
                    subject: subject.to_string(),
                    sid: sid.parse().unwrap_or(0),
                    payload,
                },
                after + len + 2,
            ))
        }
        "PING" => Some((Op::Ping, after)),
        "PONG" => Some((Op::Pong, after)),
        "+OK" => Some((Op::Ok, after)),
        "-ERR" => Some((
            Op::Err(line[4..].trim().trim_matches('\'').to_string()),
            after,
        )),
        "INFO" => {
            let json = line.find('{').map(|i| &line[i..]).unwrap_or("{}");
            Some((
                Op::Info(serde_json::from_str(json).unwrap_or(Value::Null)),
                after,
            ))
        }
        _ => Some((Op::Other(line), after)),
    }
}

/// Web `create_connect`.
pub fn connect_frame(jwt: &str, nkey: Option<&str>, sig: Option<&str>) -> String {
    let mut o = json!({
        "verbose": false,
        "pedantic": false,
        "tls_required": true,
        "jwt": jwt,
        "protocol": 1,
        "version": "2.10.18",
        "lang": "python3",
        "name": "nats.py",
        "headers": true,
        "no_responders": true,
    });
    if let (Some(k), Some(map)) = (nkey, o.as_object_mut()) {
        map.insert("nkey".into(), json!(k));
    }
    if let (Some(s), Some(map)) = (sig, o.as_object_mut()) {
        map.insert("sig".into(), json!(s));
    }
    format!("CONNECT {}\r\n", o)
}

// ---------------------------------------------------------------------------
// Connect: socket token and handshake request
// ---------------------------------------------------------------------------

/// Credentials minted by one `prepare` for the connection that follows.
pub struct Minted {
    jwt: Secret,
    subscription: String,
    key: Option<KeyPair>,
}

/// Socket token for a fresh key pair; falls back to the auth token
/// (`direct_auth`, no signature) like the web.
async fn socket_token(http: &reqwest::Client, url: &str, auth: &str) -> Minted {
    let key = KeyPair::generate();
    let resp = http
        .post(url)
        .timeout(TOKEN_TIMEOUT)
        .header("x-request-id", uuid::Uuid::new_v4().to_string())
        .header("Authorization", format!("Bearer {}", auth))
        .header("Content-Type", "application/json")
        .header("x-client-id", "growwapi")
        .header("x-client-platform", "growwapi-python-client")
        .header("x-client-platform-version", "0.0.8")
        .header("x-api-version", "1.0")
        .json(&json!({"socketKey": key.public_key()}))
        .send()
        .await;
    if let Ok(r) = resp {
        let status = r.status();
        if status.is_success() {
            if let Ok(v) = r.json::<Value>().await {
                let token = v.get("token").and_then(Value::as_str).unwrap_or("");
                let sub = v
                    .get("subscriptionId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !token.is_empty() {
                    return Minted {
                        jwt: Secret::new(token),
                        subscription: sub.to_string(),
                        key: Some(key),
                    };
                }
            }
        }
        tracing::warn!(
            status = status.as_u16(),
            "Groww socket token not issued; using the session token directly"
        );
    } else {
        tracing::warn!("Groww socket token request failed; using the session token directly");
    }
    Minted {
        jwt: Secret::new(auth),
        subscription: "direct_auth".to_string(),
        key: None,
    }
}

fn socket_request(ws_url: &str, m: &Minted, with_protocol: bool) -> Option<WsRequest> {
    let mut req = ws_url.into_client_request().ok()?;
    let h = req.headers_mut();
    h.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", m.jwt.expose())).ok()?,
    );
    h.insert(
        "X-Subscription-Id",
        HeaderValue::from_str(&m.subscription).ok()?,
    );
    h.insert(
        "User-Agent",
        HeaderValue::from_static("Python/3.10 nats.py/2.10.18"),
    );
    h.insert("X-Client-Id", HeaderValue::from_static("nats-py"));
    h.insert("X-API-Version", HeaderValue::from_static("1.0"));
    if with_protocol {
        h.insert("Sec-WebSocket-Protocol", HeaderValue::from_static("nats"));
    }
    Some(req)
}

fn is_auth_error(m: &str) -> bool {
    let m = m.to_ascii_lowercase();
    m.contains("authoriz") || m.contains("authenticat")
}

fn refused() -> String {
    "Groww refused the live market data session. Log in to Groww again.".into()
}

// ---------------------------------------------------------------------------
// Feed side
// ---------------------------------------------------------------------------

/// Subscription subject kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Price,
    Book,
}

/// Merged state of one instrument (LTP topic + book topic).
#[derive(Debug, Clone, Default)]
struct Merged {
    ltp: f64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: i64,
    ltt: i64,
    buy: Vec<DepthLevel>,
    sell: Vec<DepthLevel>,
    seen: bool,
}

struct Inst {
    sub: FeedSubscription,
    sids: Vec<u64>,
    merged: Merged,
}

type Key = (String, String);

pub struct GrowwFeed {
    http: reqwest::Client,
    auth_token: Secret,
    endpoints: FeedEndpoints,
    /// Socket token and key pair for the current connection.
    minted: Option<Minted>,
    /// Ask for the `nats` subprotocol (dropped when the server does not
    /// echo it).
    with_protocol: bool,
    /// Bytes of a partial NATS op (bounded by `MAX_PENDING_BYTES`).
    buf: Vec<u8>,
    connect_sent: bool,
    next_sid: u64,
    instruments: HashMap<Key, Inst>,
    sids: HashMap<u64, (Key, Kind)>,
}

fn is_index(exchange: &str) -> bool {
    exchange.ends_with("_INDEX")
}

/// Subjects for a subscription (web `format_topic_for_groww` as used by
/// `subscribe_batch`).
pub fn subjects(sub: &FeedSubscription) -> Vec<String> {
    subjects_with_kind(sub)
        .into_iter()
        .map(|(s, _)| s)
        .collect()
}

fn subjects_with_kind(sub: &FeedSubscription) -> Vec<(String, Kind)> {
    let ex = super::mapping::groww_exchange(&sub.exchange).to_ascii_lowercase();
    let seg = if matches!(sub.exchange.as_str(), "NFO" | "BFO") {
        "fo"
    } else {
        "eq"
    };
    let token = if sub.exchange == "NSE_INDEX" {
        sub.symbol.as_str()
    } else {
        sub.token.as_str()
    };
    let price = (format!("/ld/{}/{}/price.{}", seg, ex, token), Kind::Price);
    if sub.mode == FeedMode::Depth && !is_index(&sub.exchange) {
        vec![
            price,
            (format!("/ld/{}/{}/book.{}", seg, ex, token), Kind::Book),
        ]
    } else {
        vec![price]
    }
}

fn levels(side: &[proto::DepthLevel]) -> Vec<DepthLevel> {
    side.iter()
        .take(5)
        .filter_map(|l| {
            let pq = l.price_qty.as_ref()?;
            let lvl = DepthLevel {
                price: pq.price,
                quantity: pq.quantity as i64,
                orders: l.orders,
            };
            (lvl.price > 0.0 || lvl.quantity > 0).then_some(lvl)
        })
        .collect()
}

impl GrowwFeed {
    pub fn new(http: reqwest::Client, auth_token: &str, endpoints: FeedEndpoints) -> Self {
        Self {
            http,
            auth_token: Secret::new(auth_token),
            endpoints,
            minted: None,
            with_protocol: true,
            buf: Vec::new(),
            connect_sent: false,
            next_sid: 1,
            instruments: HashMap::new(),
            sids: HashMap::new(),
        }
    }

    fn forget(&mut self, key: &Key) -> Option<Inst> {
        let inst = self.instruments.remove(key)?;
        for sid in &inst.sids {
            self.sids.remove(sid);
        }
        Some(inst)
    }

    /// Instruments currently registered (bounded by the manager's registry).
    pub fn instrument_count(&self) -> usize {
        self.instruments.len()
    }

    fn on_msg(&mut self, sid: u64, payload: &[u8]) -> Vec<FeedEvent> {
        let Some((key, _kind)) = self.sids.get(&sid).cloned() else {
            return Vec::new();
        };
        let Some(inst) = self.instruments.get_mut(&key) else {
            return Vec::new();
        };
        let Some(data) = proto::decode(payload) else {
            tracing::debug!("Groww feed payload could not be decoded");
            return Vec::new();
        };
        let m = &mut inst.merged;
        let mut is_price = false;
        if let Some(p) = &data.ltp_data {
            is_price = true;
            m.ltp = p.ltp;
            for (dst, v) in [
                (&mut m.open, p.open),
                (&mut m.high, p.high),
                (&mut m.low, p.low),
                (&mut m.close, p.close),
            ] {
                if v != 0.0 {
                    *dst = v;
                }
            }
            if p.volume != 0.0 {
                m.volume = p.volume as i64;
            }
            if p.ts_in_millis > 0.0 {
                m.ltt = p.ts_in_millis as i64;
            }
            m.seen = true;
        }
        if let Some(i) = &data.index_data {
            is_price = true;
            m.ltp = i.value;
            if i.ts_in_millis > 0.0 {
                m.ltt = i.ts_in_millis as i64;
            }
            m.seen = true;
        }
        let mut is_book = false;
        if let Some(d) = &data.depth_data {
            is_book = true;
            m.buy = levels(&d.buy);
            m.sell = levels(&d.sell);
            if d.ts_in_millis > 0.0 {
                m.ltt = d.ts_in_millis as i64;
            }
            m.seen = true;
        }
        let mode = inst.sub.mode;
        let depth_mode = mode == FeedMode::Depth && !is_index(&inst.sub.exchange);
        if !depth_mode && !is_price {
            // LTP / quote subscriptions ignore book ticks.
            return Vec::new();
        }
        if depth_mode && !(is_price || is_book) {
            return Vec::new();
        }
        let now = now_ms();
        let effective = if is_index(&inst.sub.exchange) && mode == FeedMode::Depth {
            FeedMode::Ltp
        } else {
            mode
        };
        let mut tick = NormalizedTick {
            symbol: inst.sub.symbol.clone(),
            exchange: inst.sub.exchange.clone(),
            mode: mode.code(),
            ltp: m.ltp,
            last_trade_time_ms: m.ltt,
            timestamp_ms: now,
            ..Default::default()
        };
        if effective != FeedMode::Ltp {
            tick.open = m.open;
            tick.high = m.high;
            tick.low = m.low;
            tick.close = m.close;
            tick.volume = m.volume;
            tick.derive_change();
        }
        let mut out = Vec::with_capacity(2);
        if depth_mode {
            tick.total_buy_quantity = m.buy.iter().map(|l| l.quantity).sum();
            tick.total_sell_quantity = m.sell.iter().map(|l| l.quantity).sum();
            let depth = NormalizedDepth {
                symbol: tick.symbol.clone(),
                exchange: tick.exchange.clone(),
                ltp: m.ltp,
                buy: m.buy.clone(),
                sell: m.sell.clone(),
                total_buy_quantity: tick.total_buy_quantity,
                total_sell_quantity: tick.total_sell_quantity,
                timestamp_ms: now,
            };
            out.push(FeedEvent::Tick(tick));
            out.push(FeedEvent::Depth(depth));
        } else {
            out.push(FeedEvent::Tick(tick));
        }
        out
    }

    /// Use these credentials for the next handshake (tests drive the NATS
    /// handshake without a socket-token server).
    pub fn set_minted(&mut self, jwt: &str, key: Option<KeyPair>) {
        self.minted = Some(Minted {
            jwt: Secret::new(jwt),
            subscription: "direct_auth".into(),
            key,
        });
    }

    /// The `CONNECT` reply to the server's `INFO`, signed over its nonce.
    fn connect_reply(&self, info: &Value) -> String {
        let nonce = info.get("nonce").and_then(Value::as_str).unwrap_or("");
        let (jwt, key) = match &self.minted {
            Some(m) => (m.jwt.expose().to_string(), m.key.as_ref()),
            None => (self.auth_token.expose().to_string(), None),
        };
        let (nkey, sig) = match (key, nonce.is_empty()) {
            (Some(k), false) => (Some(k.public_key()), Some(k.sign_nonce(nonce))),
            _ => (None, None),
        };
        connect_frame(&jwt, nkey.as_deref(), sig.as_deref())
    }

    /// Buffer one frame and handle every complete NATS op in it.
    fn parse_ops(&mut self, data: &[u8]) -> Vec<FeedEvent> {
        self.buf.extend_from_slice(data);
        if self.buf.len() > MAX_PENDING_BYTES {
            tracing::warn!("Groww feed sent an oversized frame; dropping it");
            self.buf.clear();
            return Vec::new();
        }
        let buf = std::mem::take(&mut self.buf);
        let mut out = Vec::new();
        let mut used = 0;
        while let Some((op, n)) = next_op(&buf[used..]) {
            used += n;
            match op {
                Op::Info(info) => {
                    let connect = self.connect_reply(&info);
                    out.push(FeedEvent::Reply(Message::Text(connect)));
                    out.push(FeedEvent::Reply(Message::Text("PING\r\n".into())));
                    self.connect_sent = true;
                }
                Op::Ping => out.push(FeedEvent::Reply(Message::Text("PONG\r\n".into()))),
                Op::Pong => {
                    out.push(FeedEvent::Heartbeat);
                    if self.connect_sent {
                        out.push(FeedEvent::AuthOk);
                    }
                }
                Op::Ok => out.push(FeedEvent::AuthOk),
                Op::Err(m) => {
                    if is_auth_error(&m) {
                        tracing::warn!("Groww feed refused the session: {}", m);
                        out.push(FeedEvent::AuthFailed(refused()));
                    } else {
                        tracing::warn!("Groww feed error: {}", m);
                    }
                }
                Op::Msg { sid, payload, .. } => out.extend(self.on_msg(sid, &payload)),
                Op::Other(line) => tracing::debug!("Groww feed op ignored: {}", line),
            }
        }
        self.buf = buf;
        self.buf.drain(..used);
        out
    }
}

#[async_trait]
impl BrokerFeed for GrowwFeed {
    fn broker(&self) -> &'static str {
        "groww"
    }

    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        if self.auth_token.expose().trim().is_empty() {
            return Err(PrepareError::AuthFailed(refused()));
        }
        self.minted = Some(
            socket_token(
                &self.http,
                &self.endpoints.socket_token_url,
                self.auth_token.expose(),
            )
            .await,
        );
        self.with_protocol = true;
        Ok(())
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let fallback;
        let m = match &self.minted {
            Some(m) => m,
            None => {
                fallback = Minted {
                    jwt: self.auth_token.clone(),
                    subscription: "direct_auth".into(),
                    key: None,
                };
                &fallback
            }
        };
        socket_request(&self.endpoints.ws_url, m, self.with_protocol).ok_or_else(|| {
            tracing::error!("Groww feed request could not be built");
            AppError::Broker("Groww live data could not be started. Log in to Groww again.".into())
        })
    }

    fn on_connect_failed(&mut self, error: &str) -> bool {
        // The server did not echo the `nats` subprotocol: retry without it.
        if self.with_protocol && error.to_ascii_lowercase().contains("subprotocol") {
            self.with_protocol = false;
            return true;
        }
        false
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // NATS sids are per connection; everything is re-subscribed once
        // the server accepts the session.
        self.instruments.clear();
        self.sids.clear();
        self.next_sid = 1;
        self.buf.clear();
        self.connect_sent = false;
        Vec::new()
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn auth_ack_timeout(&self) -> Option<Duration> {
        Some(ASSUME_READY_AFTER)
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((NATS_PING_EVERY, Message::Text("PING\r\n".into())))
    }

    fn is_auth_failure(&self, http_status: Option<u16>) -> bool {
        matches!(http_status, Some(401) | Some(403))
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut text = String::new();
        for s in subs {
            let key = s.instrument_key();
            self.forget(&key);
            let mut sids = Vec::new();
            for (subject, kind) in subjects_with_kind(s) {
                let sid = self.next_sid;
                self.next_sid += 1;
                self.sids.insert(sid, (key.clone(), kind));
                sids.push(sid);
                text.push_str(&format!("SUB {} {}\r\n", subject, sid));
            }
            self.instruments.insert(
                key,
                Inst {
                    sub: s.clone(),
                    sids,
                    merged: Merged::default(),
                },
            );
        }
        if text.is_empty() {
            return Vec::new();
        }
        text.push_str("PING\r\n");
        vec![Message::Text(text)]
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut text = String::new();
        for s in subs {
            if let Some(inst) = self.forget(&s.instrument_key()) {
                for sid in inst.sids {
                    text.push_str(&format!("UNSUB {}\r\n", sid));
                }
            }
        }
        if text.is_empty() {
            Vec::new()
        } else {
            vec![Message::Text(text)]
        }
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_ops(t.as_bytes()),
            Message::Binary(b) => self.parse_ops(b),
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}
