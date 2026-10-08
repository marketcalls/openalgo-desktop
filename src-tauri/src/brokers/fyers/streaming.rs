//! Fyers streaming (web `streaming/fyers_hsm_websocket.py`,
//! `fyers_mapping.py`, `fyers_token_converter.py`, `fyers_adapter.py`,
//! `fyers_tbt_websocket.py`, `fyers_order_adapter.py`).
//!
//! HSM market data (`HsmFeed`):
//! * `wss://socket.fyers.in/hsm/v1-5/prod` with **no** `Authorization`
//!   header (the gateway silently drops a handshake that carries one,
//!   `fyers_hsm_websocket.py` `_run_websocket`).
//! * Auth is in-band: a binary request-type-1 frame carrying the `hsm_key`
//!   claim of the access-token JWT; the reply's first field is `"K"` when
//!   accepted (`_auth_response_ok`). Subscriptions wait for it.
//! * Topics: `sf|<seg>|<fytoken[10:]>` (LTP/quote), `dp|<seg>|<fytoken[10:]>`
//!   (5-level depth), `if|<seg>|<index display name>` (indices, any mode);
//!   `<seg>` from `fytoken[:4]` (`fyers_token_converter.py`
//!   `_convert_to_hsm_token`). The fytoken is the master's token column.
//! * Data frames: type 6, scrip count `[7:9]` big-endian, then 83 snapshot /
//!   85 update records (`_parse_data_feed`). Values are big-endian `i32`
//!   with `-2147483648` meaning absent; the topic id is read in host
//!   (little-endian) order, as the web does.
//! * Prices: `value / multiplier / 100` for scrips and depth, `value / 100`
//!   for indices, rounded to the frame's precision (`fyers_mapping.py`).
//! * HSM has no selective unsubscribe (web `unsubscribe_symbols`); an
//!   unsubscribed topic is dropped locally and is gone after a reconnect.
//!
//! TBT 50-level depth (`TbtFeed`): `wss://rtsocket-api.fyers.in/versova`,
//! `authorization: app_id:access_token`, JSON subscribe, protobuf
//! `SocketMessage` frames (`msg.proto`), text `ping` every 10 s.
//!
//! Order updates (`OrderFeed`): `wss://socket.fyers.in/trade/v3`,
//! `Authorization: app_id:access_token`, `{"T":"SUB_ORD",...}` after open.

use super::mapping::exchange_name;
use crate::brokers::common::mpp::py_round;
use crate::brokers::common::streaming::{
    now_ms, round2, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, PrepareError, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{AuthToken, DepthLevel};
use crate::error::{AppError, Result};
use crate::security::Secret;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub const HSM_URL: &str = "wss://socket.fyers.in/hsm/v1-5/prod";
pub const TBT_URL: &str = "wss://rtsocket-api.fyers.in/versova";
pub const ORDER_WS_URL: &str = "wss://socket.fyers.in/trade/v3";

/// web `self.source`.
pub const HSM_SOURCE: &str = "OpenAlgo-HSM";
/// web subscription channel.
pub const HSM_CHANNEL: u8 = 11;
/// Absent value marker in HSM frames.
pub const HSM_ABSENT: i32 = i32::MIN;

/// web `DATA_FIELDS` (scrip feed).
pub const DATA_FIELDS: &[&str] = &[
    "ltp",
    "vol_traded_today",
    "last_traded_time",
    "exch_feed_time",
    "bid_size",
    "ask_size",
    "bid_price",
    "ask_price",
    "last_traded_qty",
    "tot_buy_qty",
    "tot_sell_qty",
    "avg_trade_price",
    "OI",
    "low_price",
    "high_price",
    "Yhigh",
    "Ylow",
    "lower_ckt",
    "upper_ckt",
    "open_price",
    "prev_close_price",
    "type",
    "symbol",
];

/// web `INDEX_FIELDS`.
pub const INDEX_FIELDS: &[&str] = &[
    "ltp",
    "prev_close_price",
    "exch_feed_time",
    "high_price",
    "low_price",
    "open_price",
    "type",
    "symbol",
];

/// web `DEPTH_FIELDS`: bid prices 1-5, ask prices 1-5, bid sizes, ask
/// sizes, bid orders, ask orders, then type and symbol (32 fields).
pub const DEPTH_FIELD_COUNT: usize = 32;

/// `fytoken[:4]` -> HSM segment (web `EXCHANGE_SEGMENTS`).
pub fn hsm_segment(code: &str) -> Option<&'static str> {
    match code {
        "1010" => Some("nse_cm"),
        "1011" => Some("nse_fo"),
        "1120" => Some("mcx_fo"),
        "1210" => Some("bse_cm"),
        "1211" => Some("bse_fo"),
        "1212" => Some("bcs_fo"),
        "1012" => Some("cde_fo"),
        "1020" => Some("nse_com"),
        _ => None,
    }
}

/// Kind of an HSM topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    Scrip,
    Index,
    Depth,
}

impl Topic {
    pub fn of(name: &str) -> Option<Topic> {
        if name.starts_with("sf|") {
            Some(Topic::Scrip)
        } else if name.starts_with("if|") {
            Some(Topic::Index)
        } else if name.starts_with("dp|") {
            Some(Topic::Depth)
        } else {
            None
        }
    }

    fn field_len(self) -> usize {
        match self {
            Topic::Scrip => DATA_FIELDS.len(),
            Topic::Index => INDEX_FIELDS.len(),
            Topic::Depth => DEPTH_FIELD_COUNT,
        }
    }
}

/// HSM topics for one subscription (web `_convert_to_hsm_token`): an index
/// is always `if|` (depth subscribers get synthetic depth from it); a scrip
/// is `sf|`, plus `dp|` when depth is wanted (the desktop serves every mode
/// of an instrument from one effective subscription, so a depth
/// subscription still needs the quote topic).
pub fn hsm_topics(fytoken: &str, brsymbol: &str, index_name: &str, mode: FeedMode) -> Vec<String> {
    let (Some(code), true) = (fytoken.get(..4), fytoken.len() >= 10) else {
        return Vec::new();
    };
    let Some(seg) = hsm_segment(code) else {
        return Vec::new();
    };
    if brsymbol.ends_with("-INDEX") {
        let name = if index_name.is_empty() {
            brsymbol
                .split_once(':')
                .map(|(_, s)| s)
                .unwrap_or(brsymbol)
                .replace("-INDEX", "")
        } else {
            index_name.to_string()
        };
        return vec![format!("if|{}|{}", seg, name)];
    }
    let suffix = &fytoken[10..];
    let mut v = vec![format!("sf|{}|{}", seg, suffix)];
    if mode == FeedMode::Depth {
        v.push(format!("dp|{}|{}", seg, suffix));
    }
    v
}

/// web `_create_auth_message`.
pub fn auth_frame(hsm_key: &str, source: &str) -> Vec<u8> {
    let size = 18 + hsm_key.len() + source.len();
    let mut b = Vec::with_capacity(size);
    b.extend(((size - 2) as u16).to_be_bytes());
    b.push(1); // request type: authentication
    b.push(4); // field count
    b.push(1);
    b.extend((hsm_key.len() as u16).to_be_bytes());
    b.extend(hsm_key.as_bytes());
    b.push(2);
    b.extend(1u16.to_be_bytes());
    b.push(b'P'); // production mode
    b.push(3);
    b.extend(1u16.to_be_bytes());
    b.push(1);
    b.push(4);
    b.extend((source.len() as u16).to_be_bytes());
    b.extend(source.as_bytes());
    b
}

/// web `_create_subscription_message`.
pub fn subscribe_frame(topics: &[String], channel: u8) -> Vec<u8> {
    let mut scrips = Vec::new();
    scrips.extend((topics.len() as u16).to_be_bytes());
    for t in topics {
        let bytes = t.as_bytes();
        scrips.push(bytes.len().min(255) as u8);
        scrips.extend(&bytes[..bytes.len().min(255)]);
    }
    let mut b = Vec::with_capacity(scrips.len() + 11);
    b.extend(((6 + scrips.len()) as u16).to_be_bytes());
    b.push(4); // request type: subscribe
    b.push(2); // field count
    b.push(1);
    b.extend((scrips.len() as u16).to_be_bytes());
    b.extend(scrips);
    b.push(2);
    b.extend(1u16.to_be_bytes());
    b.push(channel);
    b
}

/// web `_auth_response_ok`: field 1 of a type-1 frame is `"K"`.
pub fn auth_response_ok(data: &[u8]) -> bool {
    if data.len() < 8 {
        return false;
    }
    let len = u16::from_be_bytes([data[5], data[6]]) as usize;
    data.get(7..7 + len) == Some(b"K".as_slice())
}

#[derive(Debug, Clone)]
struct TopicSub {
    symbol: String,
    exchange: String,
    brsymbol: String,
    mode: FeedMode,
}

#[derive(Debug, Clone)]
struct TopicState {
    name: String,
    kind: Topic,
    values: Vec<Option<i32>>,
    multiplier: u16,
    precision: u8,
}

/// Bytes cursor that never panics.
struct Cursor<'a> {
    b: &'a [u8],
    off: usize,
}

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.off)?;
        self.off += 1;
        Some(v)
    }
    fn u16_be(&mut self) -> Option<u16> {
        let s = self.b.get(self.off..self.off + 2)?;
        self.off += 2;
        Some(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u16_le(&mut self) -> Option<u16> {
        let s = self.b.get(self.off..self.off + 2)?;
        self.off += 2;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    fn i32_be(&mut self) -> Option<i32> {
        let s = self.b.get(self.off..self.off + 4)?;
        self.off += 4;
        Some(i32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.off..self.off + n)?;
        self.off += n;
        Some(s)
    }
    fn skip(&mut self, n: usize) {
        self.off = (self.off + n).min(self.b.len());
    }
    fn values(&mut self, count: usize) -> Vec<Option<i32>> {
        (0..count)
            .map_while(|_| self.i32_be())
            .map(|v| (v != HSM_ABSENT).then_some(v))
            .collect()
    }
}

/// The HSM market-data feed.
pub struct HsmFeed {
    url: String,
    token: Secret,
    hsm_key: Secret,
    symbols: SymbolResolver,
    /// Subscribed topics (removed on unsubscribe).
    topics: HashMap<String, TopicSub>,
    /// Topic id -> state, for this connection (cleared on reconnect).
    live: HashMap<u16, TopicState>,
}

fn read_hsm_key(raw: &str) -> Result<String> {
    let claims = super::auth::decode_jwt_claims(raw).ok_or_else(|| {
        AppError::Auth(
            "Your Fyers session cannot be used for live market data. Log in to Fyers again.".into(),
        )
    })?;
    if claims.exp > 0 && claims.exp < chrono::Utc::now().timestamp() {
        return Err(super::session_expired());
    }
    claims.hsm_key.filter(|k| !k.is_empty()).ok_or_else(|| {
        AppError::Auth(
            "Your Fyers session cannot be used for live market data. Log in to Fyers again.".into(),
        )
    })
}

impl HsmFeed {
    pub fn new(url: &str, auth: &AuthToken, symbols: SymbolResolver) -> Result<Self> {
        let (_, access) = auth.pair().ok_or_else(super::session_expired)?;
        let hsm_key = read_hsm_key(access)?;
        Ok(Self {
            url: url.to_string(),
            token: Secret::new(access),
            hsm_key: Secret::new(hsm_key),
            symbols,
            topics: HashMap::new(),
            live: HashMap::new(),
        })
    }

    fn index_name(&self, s: &FeedSubscription) -> String {
        self.symbols
            .by_symbol(&s.exchange, &s.symbol)
            .map(|r| r.name)
            .unwrap_or_default()
    }

    fn topics_for(&self, s: &FeedSubscription) -> Vec<String> {
        let t = hsm_topics(&s.token, &s.brsymbol, &self.index_name(s), s.mode);
        if t.is_empty() {
            tracing::warn!(
                "No Fyers feed topic for {}:{} (token not in the master)",
                s.exchange,
                s.symbol
            );
        }
        t
    }

    fn register(&mut self, subs: &[FeedSubscription]) -> Vec<String> {
        let mut new = Vec::new();
        for s in subs {
            for t in self.topics_for(s) {
                let entry = TopicSub {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    brsymbol: s.brsymbol.clone(),
                    mode: s.mode,
                };
                self.topics.insert(t.clone(), entry);
                if !new.contains(&t) {
                    new.push(t);
                }
            }
        }
        new
    }

    fn frames(topics: &[String]) -> Vec<Message> {
        if topics.is_empty() {
            return Vec::new();
        }
        // Keep each frame's byte counts inside u16 range.
        topics
            .chunks(500)
            .map(|c| Message::Binary(subscribe_frame(c, HSM_CHANNEL)))
            .collect()
    }

    fn parse_binary(&mut self, data: &[u8]) -> Vec<FeedEvent> {
        if data.len() < 3 {
            return Vec::new();
        }
        match data[2] {
            1 => {
                if auth_response_ok(data) {
                    vec![FeedEvent::AuthOk]
                } else {
                    tracing::warn!("Fyers HSM refused the session key");
                    vec![FeedEvent::AuthFailed(
                        "Fyers refused the live market data session. Log in to Fyers again.".into(),
                    )]
                }
            }
            6 => self.parse_data_feed(data),
            _ => Vec::new(),
        }
    }

    fn parse_data_feed(&mut self, data: &[u8]) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        if data.len() < 9 {
            return out;
        }
        let count = u16::from_be_bytes([data[7], data[8]]);
        let mut c = Cursor { b: data, off: 9 };
        for _ in 0..count {
            let ok = match c.u8() {
                Some(83) => self.snapshot(&mut c, &mut out),
                Some(85) => self.update(&mut c, &mut out),
                _ => false,
            };
            if !ok {
                break;
            }
        }
        out
    }

    fn snapshot(&mut self, c: &mut Cursor<'_>, out: &mut Vec<FeedEvent>) -> bool {
        let (Some(id), Some(len)) = (c.u16_le(), c.u8()) else {
            return false;
        };
        let Some(name) = c
            .bytes(len as usize)
            .map(|b| String::from_utf8_lossy(b).into_owned())
        else {
            return false;
        };
        let Some(kind) = Topic::of(&name) else {
            // Unknown layout: the rest of the frame cannot be located.
            return false;
        };
        let Some(count) = c.u8() else {
            return false;
        };
        let mut values = c.values(count as usize);
        values.resize(kind.field_len().max(values.len()), None);
        values.truncate(kind.field_len());
        let (mut multiplier, mut precision) = (100u16, 2u8);
        if kind != Topic::Index {
            c.skip(2);
            match (c.u16_be(), c.u8()) {
                (Some(m), Some(p)) => {
                    multiplier = m;
                    precision = p;
                }
                _ => return false,
            }
            // exchange, exchange token, symbol
            for _ in 0..3 {
                match c.u8() {
                    Some(n) => {
                        if c.bytes(n as usize).is_none() {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
        }
        let state = TopicState {
            name,
            kind,
            values,
            multiplier,
            precision,
        };
        self.emit(&state, out);
        // Only topics this feed subscribed are kept, so the map is bounded
        // by the subscriptions.
        if self.topics.contains_key(&state.name) {
            self.live.insert(id, state);
        } else {
            self.live.remove(&id);
        }
        true
    }

    fn update(&mut self, c: &mut Cursor<'_>, out: &mut Vec<FeedEvent>) -> bool {
        let (Some(id), Some(count)) = (c.u16_le(), c.u8()) else {
            return false;
        };
        let values = c.values(count as usize);
        let Some(mut state) = self.live.remove(&id) else {
            return true;
        };
        let mut changed = false;
        for (i, v) in values.into_iter().enumerate() {
            if let (Some(v), Some(slot)) = (v, state.values.get_mut(i)) {
                if *slot != Some(v) {
                    *slot = Some(v);
                    changed = true;
                }
            }
        }
        if changed {
            self.emit(&state, out);
        }
        if self.topics.contains_key(&state.name) {
            self.live.insert(id, state);
        }
        true
    }

    fn emit(&self, st: &TopicState, out: &mut Vec<FeedEvent>) {
        let Some(sub) = self.topics.get(&st.name) else {
            return;
        };
        let now = now_ms();
        match st.kind {
            Topic::Scrip => out.push(FeedEvent::Tick(scrip_tick(sub, st, now))),
            Topic::Index => {
                let t = index_tick(sub, st, now);
                if sub.mode == FeedMode::Depth && t.ltp > 0.0 {
                    out.push(FeedEvent::Depth(synthetic_depth(&t, now)));
                }
                out.push(FeedEvent::Tick(t));
            }
            Topic::Depth => {
                if sub.mode == FeedMode::Depth {
                    out.push(FeedEvent::Depth(depth_event(sub, st, now)));
                }
            }
        }
    }
}

/// `/100` for the paise-quoted segments (every Fyers cash and derivative
/// symbol), by the exchange prefix of the Fyers symbol.
fn segment_divisor(brsymbol: &str) -> f64 {
    let ex = brsymbol.split(':').next().unwrap_or("");
    if matches!(ex, "BSE" | "MCX" | "NSE" | "NFO" | "CDS" | "BCD") {
        100.0
    } else {
        1.0
    }
}

/// web `convert_price`: 0 for absent/zero values or a zero multiplier.
fn price(v: Option<i32>, multiplier: u16, precision: u8, divisor: f64) -> f64 {
    match v {
        Some(x) if x != 0 && multiplier > 0 => py_round(
            f64::from(x) / f64::from(multiplier) / divisor,
            i32::from(precision),
        ),
        _ => 0.0,
    }
}

fn int(v: Option<i32>) -> i64 {
    v.map(i64::from).unwrap_or(0)
}

fn field(st: &TopicState, i: usize) -> Option<i32> {
    st.values.get(i).copied().flatten()
}

fn scrip_tick(sub: &TopicSub, st: &TopicState, now: i64) -> NormalizedTick {
    let div = segment_divisor(&sub.brsymbol);
    let p = |i| price(field(st, i), st.multiplier, st.precision, div);
    let mut t = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode: sub.mode.code(),
        ltp: p(0),
        volume: int(field(st, 1)),
        last_trade_time_ms: int(field(st, 2)) * 1000,
        last_quantity: int(field(st, 8)),
        total_buy_quantity: int(field(st, 9)),
        total_sell_quantity: int(field(st, 10)),
        average_price: p(11),
        oi: int(field(st, 12)),
        low: p(13),
        high: p(14),
        open: p(19),
        close: p(20),
        timestamp_ms: now,
        ..Default::default()
    };
    t.derive_change();
    t
}

fn index_tick(sub: &TopicSub, st: &TopicState, now: i64) -> NormalizedTick {
    // Index frames carry no multiplier: web divides by its default 100.
    let p = |i| price(field(st, i), 100, 2, 1.0);
    let mut t = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode: sub.mode.code(),
        ltp: p(0),
        close: p(1),
        last_trade_time_ms: int(field(st, 2)) * 1000,
        high: p(3),
        low: p(4),
        open: p(5),
        timestamp_ms: now,
        ..Default::default()
    };
    t.derive_change();
    t
}

/// web `map_to_openalgo_depth`: levels with a price only; LTP is the mid of
/// the best bid and ask.
fn depth_event(sub: &TopicSub, st: &TopicState, now: i64) -> NormalizedDepth {
    let div = segment_divisor(&sub.brsymbol);
    let p = |i| price(field(st, i), st.multiplier, st.precision, div);
    let mut buy = Vec::new();
    let mut sell = Vec::new();
    for i in 0..5 {
        let bp = p(i);
        if bp > 0.0 {
            buy.push(DepthLevel {
                price: bp,
                quantity: int(field(st, 10 + i)),
                orders: int(field(st, 20 + i)),
            });
        }
        let ap = p(5 + i);
        if ap > 0.0 {
            sell.push(DepthLevel {
                price: ap,
                quantity: int(field(st, 15 + i)),
                orders: int(field(st, 25 + i)),
            });
        }
    }
    let ltp = match (buy.first(), sell.first()) {
        (Some(b), Some(s)) => (b.price + s.price) / 2.0,
        _ => 0.0,
    };
    NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        ltp,
        total_buy_quantity: buy.iter().map(|l| l.quantity).sum(),
        total_sell_quantity: sell.iter().map(|l| l.quantity).sum(),
        buy,
        sell,
        timestamp_ms: now,
    }
}

/// web `map_index_to_synthetic_depth`: five levels 0.05% apart around LTP.
pub fn synthetic_depth(t: &NormalizedTick, now: i64) -> NormalizedDepth {
    let spread = t.ltp * 5.0 / 10_000.0;
    let level = |i: usize, sign: f64| DepthLevel {
        price: round2(t.ltp + sign * spread * (i as f64 + 1.0)),
        quantity: 1000 * (6 - i as i64),
        orders: 1,
    };
    NormalizedDepth {
        symbol: t.symbol.clone(),
        exchange: t.exchange.clone(),
        ltp: t.ltp,
        buy: (0..5).map(|i| level(i, -1.0)).collect(),
        sell: (0..5).map(|i| level(i, 1.0)).collect(),
        total_buy_quantity: 0,
        total_sell_quantity: 0,
        timestamp_ms: now,
    }
}

impl BrokerFeed for HsmFeed {
    fn broker(&self) -> &'static str {
        "fyers"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        // Re-check expiry on every (re)connect so a rolled-over token stops
        // the reconnect loop instead of hammering the gateway.
        read_hsm_key(self.token.expose())?;
        let mut req = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Fyers feed address is invalid".into()))?;
        req.headers_mut()
            .insert("User-Agent", HeaderValue::from_static("OpenAlgo-HSM/1.0"));
        Ok(req)
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.live.clear();
        vec![Message::Binary(auth_frame(
            self.hsm_key.expose(),
            HSM_SOURCE,
        ))]
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let topics = self.register(subs);
        Self::frames(&topics)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        for s in subs {
            for t in self.topics_for(&s.with_mode(FeedMode::Depth)) {
                self.topics.remove(&t);
            }
        }
        self.live.retain(|_, st| self.topics.contains_key(&st.name));
        Vec::new()
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        let before: HashSet<String> = self.topics_for(old).into_iter().collect();
        let after = self.topics_for(new);
        for t in before.iter().filter(|t| !after.contains(t)) {
            self.topics.remove(t);
        }
        self.live.retain(|_, st| self.topics.contains_key(&st.name));
        let added: Vec<FeedSubscription> = vec![new.clone()];
        let registered = self.register(&added);
        let fresh: Vec<String> = registered
            .into_iter()
            .filter(|t| !before.contains(t))
            .collect();
        Self::frames(&fresh)
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        // web `ping_interval=30`: protocol pings keep a quiet socket alive
        // and the pongs feed the stall watchdog.
        Some((Duration::from_secs(30), Message::Ping(Vec::new())))
    }
}

// ---------------------------------------------------------------------------
// Order updates
// ---------------------------------------------------------------------------

/// web `_STATUS_MAP` (order socket): transit and pending are both open.
pub fn order_feed_status(code: i64) -> String {
    match code {
        1 => "cancelled".into(),
        2 => "complete".into(),
        4 | 6 => "open".into(),
        5 => "rejected".into(),
        7 => "expired".into(),
        other => other.to_string(),
    }
}

fn order_feed_exchange(d: &Value) -> String {
    let n = |k: &str| d.get(k).and_then(Value::as_i64).unwrap_or(-1);
    let pair = (n("exchange"), n("segment"));
    let mapped = match pair {
        (12, 12) => Some("BCD"),
        (11, 11) => Some("MCX"),
        (e, s) => exchange_name(e, s),
    };
    match mapped {
        Some(m) => m.to_string(),
        None => {
            let sym = d.get("symbol").and_then(Value::as_str).unwrap_or("");
            match sym.split_once(':') {
                Some((ex, _)) => ex.to_string(),
                None => d
                    .get("exchange")
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default(),
            }
        }
    }
}

fn pick<'a>(d: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names
        .iter()
        .filter_map(|n| d.get(*n))
        .find(|v| !v.is_null())
}

/// web `_unwrap_order_record` + `normalize`.
pub fn parse_order_update(text: &str, symbols: &SymbolResolver) -> Option<OrderUpdate> {
    let msg: Value = serde_json::from_str(text).ok()?;
    if !msg.is_object() {
        return None;
    }
    let status_keys = ["org_ord_status", "status", "ord_status"];
    let is_record = |c: &Value| {
        c.is_object()
            && c.get("id").is_some_and(|v| !v.is_null())
            && pick(c, &status_keys).is_some()
    };
    let d = ["orders", "d", "data"]
        .iter()
        .filter_map(|k| msg.get(*k))
        .chain(std::iter::once(&msg))
        .find(|c| is_record(c))?;
    let num = |names: &[&str]| pick(d, names).map(super::mapping::num).unwrap_or(0.0);
    let text_of = |v: Option<&Value>| match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    let raw_status = pick(d, &status_keys);
    let status = match raw_status.and_then(Value::as_i64) {
        Some(c) => order_feed_status(c),
        None => text_of(raw_status),
    };
    let qty = num(&["qty"]) as i64;
    let filled = num(&["qty_filled", "filledQty"]) as i64;
    let exchange = order_feed_exchange(d);
    let br = text_of(pick(d, &["symbol"]));
    let side = pick(d, &["tran_side", "side"]);
    let pt = pick(d, &["ord_type", "type"]);
    Some(OrderUpdate {
        orderid: text_of(pick(d, &["id"])),
        symbol: symbols.oa_symbol_or_raw(&br, &exchange),
        exchange,
        action: match side.and_then(Value::as_i64) {
            Some(1) => "BUY".into(),
            Some(-1) => "SELL".into(),
            _ => text_of(side),
        },
        quantity: qty,
        price: num(&["price_limit", "limitPrice"]),
        trigger_price: num(&["price_stop", "stopPrice"]),
        pricetype: match pt.and_then(Value::as_i64) {
            Some(c @ 1..=4) => super::mapping::pricetype_name(c),
            _ => text_of(pt),
        },
        // The web passes the Fyers product code through unchanged here.
        product: text_of(pick(d, &["product_type", "productType"])),
        rejection_reason: if raw_status.and_then(Value::as_i64) == Some(5) {
            text_of(pick(d, &["oms_msg", "message", "status_msg"]))
        } else {
            String::new()
        },
        order_status: status,
        filled_quantity: filled,
        pending_quantity: (qty - filled).max(0),
        average_price: num(&["price_traded", "tradedPrice"]),
    })
}

/// The order-update socket.
pub struct OrderFeed {
    url: String,
    authorization: Secret,
    symbols: SymbolResolver,
}

impl OrderFeed {
    pub fn new(url: &str, auth: &AuthToken, symbols: SymbolResolver) -> Result<Self> {
        auth.pair().ok_or_else(super::session_expired)?;
        Ok(Self {
            url: url.to_string(),
            authorization: Secret::new(auth.raw()),
            symbols,
        })
    }
}

fn request_with_auth(url: &str, name: &'static str, value: &str) -> Result<WsRequest> {
    let mut req = url
        .into_client_request()
        .map_err(|_| AppError::Internal("Fyers feed address is invalid".into()))?;
    let v = HeaderValue::from_str(value).map_err(|_| super::session_expired())?;
    req.headers_mut().insert(name, v);
    Ok(req)
}

impl BrokerFeed for OrderFeed {
    fn broker(&self) -> &'static str {
        "fyers"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request_with_auth(&self.url, "Authorization", self.authorization.expose())
    }

    fn on_connected(&mut self) -> Vec<Message> {
        vec![Message::Text(
            json!({"T": "SUB_ORD", "SLIST": ["orders"], "SUB_T": 1}).to_string(),
        )]
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => parse_order_update(t, &self.symbols)
                .map(|u| vec![FeedEvent::OrderUpdate(u)])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        // web base order adapter: protocol ping every 20 s.
        Some((Duration::from_secs(20), Message::Ping(Vec::new())))
    }
}

// ---------------------------------------------------------------------------
// TBT 50-level depth (protobuf)
// ---------------------------------------------------------------------------

/// Minimal protobuf reader for `msg.proto` (wire types 0, 1, 2, 5).
pub mod proto {
    /// One decoded field.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum Field<'a> {
        Varint(u64),
        Bytes(&'a [u8]),
        Fixed64(u64),
        Fixed32(u32),
    }

    pub fn varint(b: &[u8], pos: &mut usize) -> Option<u64> {
        let mut v: u64 = 0;
        for shift in (0..64).step_by(7) {
            let byte = *b.get(*pos)?;
            *pos += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// All `(field number, value)` pairs of a message; `None` if malformed.
    pub fn fields(b: &[u8]) -> Option<Vec<(u32, Field<'_>)>> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < b.len() {
            let key = varint(b, &mut pos)?;
            let (num, wire) = ((key >> 3) as u32, key & 7);
            let f = match wire {
                0 => Field::Varint(varint(b, &mut pos)?),
                1 => {
                    let s = b.get(pos..pos + 8)?;
                    pos += 8;
                    Field::Fixed64(u64::from_le_bytes(s.try_into().ok()?))
                }
                2 => {
                    let len = varint(b, &mut pos)? as usize;
                    let s = b.get(pos..pos.checked_add(len)?)?;
                    pos += len;
                    Field::Bytes(s)
                }
                5 => {
                    let s = b.get(pos..pos + 4)?;
                    pos += 4;
                    Field::Fixed32(u32::from_le_bytes(s.try_into().ok()?))
                }
                _ => return None,
            };
            out.push((num, f));
        }
        Some(out)
    }

    /// Value of a `google.protobuf.*Value` wrapper (absent field = 0).
    pub fn wrapper(b: &[u8]) -> u64 {
        fields(b)
            .and_then(|fs| {
                fs.into_iter().find_map(|(n, f)| match (n, f) {
                    (1, Field::Varint(v)) => Some(v),
                    _ => None,
                })
            })
            .unwrap_or(0)
    }
}

/// One `MarketLevel` update; `None` fields were absent on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LevelUpdate {
    pub price: Option<i64>,
    pub qty: Option<u64>,
    pub nord: Option<u64>,
    pub num: Option<u64>,
}

/// One `MarketFeed` with depth.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TbtDepthUpdate {
    pub ticker: String,
    pub token: String,
    pub tbq: Option<u64>,
    pub tsq: Option<u64>,
    pub bids: Vec<LevelUpdate>,
    pub asks: Vec<LevelUpdate>,
    pub feed_time: u64,
}

/// A decoded `SocketMessage`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TbtMessage {
    pub snapshot: bool,
    pub error: bool,
    pub msg: String,
    pub feeds: Vec<TbtDepthUpdate>,
}

fn level(b: &[u8]) -> Option<LevelUpdate> {
    use proto::Field;
    let mut l = LevelUpdate::default();
    for (n, f) in proto::fields(b)? {
        if let Field::Bytes(w) = f {
            let v = proto::wrapper(w);
            match n {
                1 => l.price = Some(v as i64),
                2 => l.qty = Some(v),
                3 => l.nord = Some(v),
                4 => l.num = Some(v),
                _ => {}
            }
        }
    }
    Some(l)
}

fn market_feed(key: &str, b: &[u8]) -> Option<Option<TbtDepthUpdate>> {
    use proto::Field;
    let mut u = TbtDepthUpdate {
        token: key.to_string(),
        ..Default::default()
    };
    let mut has_depth = false;
    for (n, f) in proto::fields(b)? {
        match (n, f) {
            (5, Field::Bytes(d)) => {
                has_depth = true;
                for (dn, df) in proto::fields(d)? {
                    match (dn, df) {
                        (1, Field::Bytes(w)) => u.tbq = Some(proto::wrapper(w)),
                        (2, Field::Bytes(w)) => u.tsq = Some(proto::wrapper(w)),
                        (3, Field::Bytes(l)) => u.asks.push(level(l)?),
                        (4, Field::Bytes(l)) => u.bids.push(level(l)?),
                        _ => {}
                    }
                }
            }
            (6, Field::Bytes(w)) => u.feed_time = proto::wrapper(w),
            (11, Field::Bytes(t)) => u.ticker = String::from_utf8_lossy(t).into_owned(),
            _ => {}
        }
    }
    if u.ticker.is_empty() {
        u.ticker = u.token.clone();
    }
    Some(has_depth.then_some(u))
}

/// Decode a TBT `SocketMessage` (feeds without depth are dropped).
pub fn decode_tbt(b: &[u8]) -> Option<TbtMessage> {
    use proto::Field;
    let mut m = TbtMessage::default();
    for (n, f) in proto::fields(b)? {
        match (n, f) {
            (2, Field::Bytes(entry)) => {
                let mut key = String::new();
                let mut value: Option<&[u8]> = None;
                for (en, ef) in proto::fields(entry)? {
                    match (en, ef) {
                        (1, Field::Bytes(k)) => key = String::from_utf8_lossy(k).into_owned(),
                        (2, Field::Bytes(v)) => value = Some(v),
                        _ => {}
                    }
                }
                if let Some(v) = value {
                    if let Some(u) = market_feed(&key, v)? {
                        m.feeds.push(u);
                    }
                }
            }
            (3, Field::Varint(v)) => m.snapshot = v != 0,
            (4, Field::Bytes(s)) => m.msg = String::from_utf8_lossy(s).into_owned(),
            (5, Field::Varint(v)) => m.error = v != 0,
            _ => {}
        }
    }
    Some(m)
}

/// Exchanges with 50-level depth (web `TBT_SUPPORTED_EXCHANGES`).
pub const TBT_EXCHANGES: &[&str] = &["NSE", "NFO"];
pub const TBT_LEVELS: usize = 50;

#[derive(Debug, Clone)]
struct Book {
    buy: Vec<DepthLevel>,
    sell: Vec<DepthLevel>,
    tbq: i64,
    tsq: i64,
}

impl Book {
    fn empty() -> Self {
        Self {
            buy: vec![DepthLevel::default(); TBT_LEVELS],
            sell: vec![DepthLevel::default(); TBT_LEVELS],
            tbq: 0,
            tsq: 0,
        }
    }
}

/// Where to look up the TBT socket address before connecting.
struct TbtLookup {
    http: reqwest::Client,
    url: String,
}

/// The TBT 50-level depth feed.
pub struct TbtFeed {
    url: String,
    lookup: Option<TbtLookup>,
    authorization: Secret,
    /// ticker -> (symbol, exchange)
    subs: HashMap<String, (String, String)>,
    /// ticker -> book (one per subscription, removed with it)
    books: HashMap<String, Book>,
    channel_resumed: bool,
}

impl TbtFeed {
    pub fn new(url: &str, auth: &AuthToken, _symbols: SymbolResolver) -> Result<Self> {
        auth.pair().ok_or_else(super::session_expired)?;
        Ok(Self {
            url: url.to_string(),
            lookup: None,
            authorization: Secret::new(auth.raw()),
            subs: HashMap::new(),
            books: HashMap::new(),
            channel_resumed: false,
        })
    }

    /// Ask Fyers for the socket address before each connect (web
    /// `_get_tbt_url`); `url` stays the fallback.
    pub fn with_lookup(mut self, http: reqwest::Client, lookup_url: String) -> Self {
        self.lookup = Some(TbtLookup {
            http,
            url: lookup_url,
        });
        self
    }

    fn sub_frame(tickers: &[String], subs: i32) -> Message {
        Message::Text(
            json!({
                "type": 1,
                "data": {"subs": subs, "symbols": tickers, "mode": "depth", "channel": "1"}
            })
            .to_string(),
        )
    }

    fn switch_frame() -> Message {
        Message::Text(
            json!({"type": 2, "data": {"resumeChannels": ["1"], "pauseChannels": []}}).to_string(),
        )
    }

    /// web `_extract_depth`: a snapshot resets the book; levels are placed
    /// by their `num`; prices are paise.
    fn apply(&mut self, u: &TbtDepthUpdate, snapshot: bool) -> Option<NormalizedDepth> {
        let (symbol, exchange) = self.subs.get(&u.ticker)?.clone();
        let book = self
            .books
            .entry(u.ticker.clone())
            .or_insert_with(Book::empty);
        if snapshot {
            book.buy = vec![DepthLevel::default(); TBT_LEVELS];
            book.sell = vec![DepthLevel::default(); TBT_LEVELS];
        }
        let put = |side: &mut Vec<DepthLevel>, l: &LevelUpdate| {
            let Some(idx) = l.num.map(|n| n as usize).filter(|n| *n < TBT_LEVELS) else {
                return;
            };
            if let Some(p) = l.price {
                side[idx].price = p as f64 / 100.0;
            }
            if let Some(q) = l.qty {
                side[idx].quantity = q as i64;
            }
            if let Some(o) = l.nord {
                side[idx].orders = o as i64;
            }
        };
        for l in &u.bids {
            put(&mut book.buy, l);
        }
        for l in &u.asks {
            put(&mut book.sell, l);
        }
        if let Some(v) = u.tbq {
            book.tbq = v as i64;
        }
        if let Some(v) = u.tsq {
            book.tsq = v as i64;
        }
        let buy: Vec<DepthLevel> = book.buy.iter().filter(|l| l.price > 0.0).copied().collect();
        let sell: Vec<DepthLevel> = book
            .sell
            .iter()
            .filter(|l| l.price > 0.0)
            .copied()
            .collect();
        let (bb, ba) = (
            buy.first().map(|l| l.price).unwrap_or(0.0),
            sell.first().map(|l| l.price).unwrap_or(0.0),
        );
        let ltp = if bb > 0.0 && ba > 0.0 {
            (bb + ba) / 2.0
        } else if bb > 0.0 {
            bb
        } else {
            ba
        };
        Some(NormalizedDepth {
            symbol,
            exchange,
            ltp: round2(ltp),
            buy,
            sell,
            total_buy_quantity: book.tbq,
            total_sell_quantity: book.tsq,
            timestamp_ms: now_ms(),
        })
    }
}

#[async_trait::async_trait]
impl BrokerFeed for TbtFeed {
    fn broker(&self) -> &'static str {
        "fyers"
    }

    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        let Some(l) = &self.lookup else {
            return Ok(());
        };
        let resp = l
            .http
            .get(&l.url)
            .header("Authorization", self.authorization.expose())
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().as_u16() == 401 => return Err(PrepareError::AuthFailed(
                "Fyers did not accept the stored login for market depth. Log in to Fyers again."
                    .into(),
            )),
            Ok(r) if r.status().is_success() => {
                if let Ok(v) = r.json::<serde_json::Value>().await {
                    if let Some(u) = v
                        .pointer("/data/socket_url")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| s.starts_with("wss://") || s.starts_with("ws://"))
                    {
                        self.url = u.to_string();
                    }
                }
            }
            Ok(r) => tracing::warn!(
                status = r.status().as_u16(),
                "Fyers depth address lookup refused; using the default"
            ),
            Err(e) => tracing::warn!(
                "Fyers depth address lookup failed: {}",
                crate::brokers::common::redact::url_safe_error(&e)
            ),
        }
        Ok(())
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request_with_auth(&self.url, "authorization", self.authorization.expose())
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.channel_resumed = false;
        self.books.clear();
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut tickers = Vec::new();
        for s in subs {
            if !TBT_EXCHANGES.contains(&s.exchange.as_str()) {
                tracing::warn!(
                    "50-level depth is not available for {}:{}",
                    s.exchange,
                    s.symbol
                );
                continue;
            }
            self.subs
                .insert(s.brsymbol.clone(), (s.symbol.clone(), s.exchange.clone()));
            tickers.push(s.brsymbol.clone());
        }
        if tickers.is_empty() {
            return Vec::new();
        }
        let mut v = vec![Self::sub_frame(&tickers, 1)];
        if !self.channel_resumed {
            v.push(Self::switch_frame());
            self.channel_resumed = true;
        }
        v
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let tickers: Vec<String> = subs
            .iter()
            .filter(|s| self.subs.remove(&s.brsymbol).is_some())
            .map(|s| s.brsymbol.clone())
            .collect();
        for t in &tickers {
            self.books.remove(t);
        }
        if tickers.is_empty() {
            Vec::new()
        } else {
            vec![Self::sub_frame(&tickers, -1)]
        }
    }

    fn mode_change_frames(
        &mut self,
        _old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        // Every TBT subscription is depth; only a new ticker needs a frame.
        if self.subs.contains_key(&new.brsymbol) {
            Vec::new()
        } else {
            self.subscribe_frames(std::slice::from_ref(new))
        }
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) if t == "pong" => vec![FeedEvent::Heartbeat],
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<Value>(t) {
                    if v.get("error").and_then(Value::as_bool) == Some(true) {
                        let detail = v.get("msg").and_then(Value::as_str).unwrap_or("");
                        tracing::warn!("Fyers TBT subscription error: {}", detail);
                    }
                }
                Vec::new()
            }
            Message::Binary(b) => {
                let Some(m) = decode_tbt(b) else {
                    tracing::debug!("Unreadable Fyers TBT frame ({} bytes)", b.len());
                    return Vec::new();
                };
                if m.error {
                    tracing::warn!("Fyers TBT error: {}", m.msg);
                    return Vec::new();
                }
                m.feeds
                    .iter()
                    .filter_map(|u| self.apply(u, m.snapshot))
                    .map(FeedEvent::Depth)
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((Duration::from_secs(10), Message::Text("ping".into())))
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[50]
    }
}
