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
//! * Prices: `value / (10^precision * multiplier)`, the official SDK's
//!   formula, rounded to the precision (`fyers_mapping.py` `_price_scale`).
//!   Precision is per instrument: 2 for equity, F&O and commodity, 4 for
//!   currency. Index frames carry neither field, so they divide by 100.
//! * HSM has no selective unsubscribe (web `unsubscribe_symbols`); an
//!   unsubscribed topic is dropped locally and is gone after a reconnect.
//! * Nothing Fyers does not send is published (web #2318, #2323): an update
//!   frame is applied whole and published once; a depth message carries no
//!   LTP of its own (the bridge gives it the quote feed's last traded price,
//!   `feed::bridge::depth_update`, never a bid/ask midpoint); an index has
//!   no order book, so it gets no depth message, only its real value.
//!
//! TBT 50-level depth (`TbtFeed`, in `tbt`): `wss://rtsocket-api.fyers.in/versova`,
//! `authorization: app_id:access_token`, JSON subscribe, protobuf
//! `SocketMessage` frames (`msg.proto`), text `ping` every 10 s.
//!
//! Order updates (`OrderFeed`, in `order_feed`): `wss://socket.fyers.in/trade/v3`,
//! `Authorization: app_id:access_token`, `{"T":"SUB_ORD",...}` after open.
//!
//! Both are re-exported here, so every `streaming::` path holds.

mod order_feed;
mod tbt;

pub use order_feed::*;
pub use tbt::*;

use crate::brokers::common::mpp::py_round;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{AuthToken, DepthLevel};
use crate::error::{AppError, Result};
use crate::security::Secret;
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
/// is always `if|` (a depth subscriber gets its value: an index has no
/// order book); a scrip
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
        // web `_price_scale` defaults: index frames carry neither field.
        let (mut multiplier, mut precision) = (1u16, 2u8);
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
            // An index has no order book: its tick serves every mode,
            // depth included, and no levels are made up around its value
            // (web `map_index_to_depth`, #2318).
            Topic::Index => out.push(FeedEvent::Tick(index_tick(sub, st, now))),
            Topic::Depth => {
                if sub.mode == FeedMode::Depth {
                    out.push(FeedEvent::Depth(depth_event(sub, st, now)));
                }
            }
        }
    }
}

impl TopicState {
    /// Field `i` in rupees (web `_price_scale` and `convert_price`): the
    /// SDK's `raw / (10^precision * multiplier)`, rounded to the precision;
    /// 0 for an absent or zero value. A zero multiplier counts as 1, so a
    /// bad field never zeroes or skips the conversion.
    fn price(&self, i: usize) -> f64 {
        match field(self, i) {
            Some(x) if x != 0 => {
                let divisor =
                    10f64.powi(i32::from(self.precision)) * f64::from(self.multiplier.max(1));
                py_round(f64::from(x) / divisor, i32::from(self.precision))
            }
            _ => 0.0,
        }
    }
}

fn int(v: Option<i32>) -> i64 {
    v.map(i64::from).unwrap_or(0)
}

fn field(st: &TopicState, i: usize) -> Option<i32> {
    st.values.get(i).copied().flatten()
}

fn scrip_tick(sub: &TopicSub, st: &TopicState, now: i64) -> NormalizedTick {
    let p = |i| st.price(i);
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
    let p = |i| st.price(i);
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

/// web `map_to_openalgo_depth`: levels with a price only. A Fyers depth
/// message has no LTP, so none is set (0): the bridge gives the frame the
/// quote feed's last traded price, as the web's adapter borrows it
/// (`get_last_ltp`), never the bid/ask midpoint it used to invent (#2318).
fn depth_event(sub: &TopicSub, st: &TopicState, now: i64) -> NormalizedDepth {
    let p = |i| st.price(i);
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
    NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        ltp: 0.0,
        total_buy_quantity: buy.iter().map(|l| l.quantity).sum(),
        total_sell_quantity: sell.iter().map(|l| l.quantity).sum(),
        buy,
        sell,
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

/// A socket request carrying `name: value` as a header: the order and
/// depth sockets authenticate this way.
fn request_with_auth(url: &str, name: &'static str, value: &str) -> Result<WsRequest> {
    let mut req = url
        .into_client_request()
        .map_err(|_| AppError::Internal("Fyers feed address is invalid".into()))?;
    let v = HeaderValue::from_str(value).map_err(|_| super::session_expired())?;
    req.headers_mut().insert(name, v);
    Ok(req)
}
