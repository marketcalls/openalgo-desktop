//! Kotak legacy HSM market feed (web `streaming/HSWebSocketLib.py`,
//! `kotak_websocket.py`), used only when the feed-config service routes a
//! data centre to source `hs`. None has since September 2026 and Kotak's own
//! SDK dropped it, so `KotakBroker::create_feed` always builds the SFeed
//! client; this codec is kept for parity and is selected with
//! `KotakBroker::create_hsm_feed`.
//!
//! Every frame is big-endian binary: `u16 length` (excluding itself), then a
//! `u8` type.
//! * Connect (type 1): `[len][1][3 fields][fid 1][u16 len][trading token]
//!   [fid 2][u16 len][sid][fid 3][u16 len]"JS_API"`.
//! * Subscribe / unsubscribe (type 4 / 5): `[len][type][2 fields][fid 1]
//!   [u16 len][u16 count][(u8 len)("sf|nse_cm|11536")...][fid 2][u16 1]
//!   [u8 channel]`, prefixes `sf` scrip, `if` index, `dp` depth, at most 100
//!   scrips a frame.
//! * Acknowledgement (type 3): `[len][3][1][1][u16 4][u32 message number]`.
//!
//! Answers (`[u16 len][u8 type]...`): type 1 carries `K`/`N` and the
//! acknowledgement interval; type 6 is data (`[u32 message number]` when
//! acks are on, then `u16` sub-message count, each `[u16 len][u8 83 snapshot
//! | 85 update]`; a snapshot is `[u32 topic id][u8 len][name][u8 n][n x u32]
//! [u8 m][m x (u8 field, u8 len, text)]`, an update `[u32 topic id][u8 n]
//! [n x u32]`); types 4 and 5 are subscribe acks.
//!
//! Integer fields are unsigned; `0x80000000` means "not available" and
//! keeps the last value. Prices are `raw / (multiplier * 10^precision)` with
//! the multiplier and precision carried in the topic itself.

use super::data::{index_candidates, kotak_segment};
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use std::collections::HashMap;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const DEFAULT_HSM_URL: &str = "wss://mlhsm.kotaksecurities.com";
pub const MAX_SCRIPS: usize = 100;
pub const NOT_AVAILABLE: u32 = 0x8000_0000;
/// Topics remembered at most (web `_max_topics`).
pub const MAX_TOPICS: usize = 5000;

const CONNECTION: u8 = 1;
const ACK: u8 = 3;
const SUBSCRIBE: u8 = 4;
const UNSUBSCRIBE: u8 = 5;
const DATA: u8 = 6;
const SNAP: u8 = 83;
const UPDATE: u8 = 85;

fn framed(body: Vec<u8>) -> Vec<u8> {
    let mut out = (body.len() as u16).to_be_bytes().to_vec();
    out.extend(body);
    out
}

fn field(fid: u8, data: &[u8]) -> Vec<u8> {
    let mut v = vec![fid];
    v.extend((data.len() as u16).to_be_bytes());
    v.extend(data);
    v
}

/// web `prepareConnectionRequest2(jwt, sid)`.
pub fn connect_frame(token: &str, sid: &str) -> Vec<u8> {
    let mut b = vec![CONNECTION, 3];
    b.extend(field(1, token.as_bytes()));
    b.extend(field(2, sid.as_bytes()));
    b.extend(field(3, b"JS_API"));
    framed(b)
}

/// web `prepareSubsUnSubsRequest`; `None` past 100 scrips.
pub fn subscription_frame(
    scrips: &[String],
    subscribe: bool,
    prefix: &str,
    channel: u8,
) -> Option<Vec<u8>> {
    if scrips.is_empty() || scrips.len() > MAX_SCRIPS {
        return None;
    }
    let mut data = (scrips.len() as u16).to_be_bytes().to_vec();
    for s in scrips {
        let full = format!("{}|{}", prefix, s);
        data.push(full.len() as u8);
        data.extend(full.as_bytes());
    }
    let mut b = vec![if subscribe { SUBSCRIBE } else { UNSUBSCRIBE }, 2];
    b.extend(field(1, &data));
    b.extend(field(2, &[channel]));
    Some(framed(b))
}

/// web `get_acknowledgement_req`.
pub fn ack_frame(msg_num: u32) -> Vec<u8> {
    let mut b = vec![ACK, 1];
    b.extend(field(1, &msg_num.to_be_bytes()));
    framed(b)
}

/// The kind of a topic, from its name prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicKind {
    Scrip,
    Index,
    Depth,
}

/// One topic's last known values.
#[derive(Debug, Clone)]
pub struct Topic {
    pub kind: TopicKind,
    pub exchange: String,
    pub token: String,
    pub trading_symbol: String,
    values: Vec<Option<u32>>,
    multiplier: f64,
    precision: i32,
}

impl Topic {
    fn new(kind: TopicKind) -> Self {
        Self {
            kind,
            exchange: String::new(),
            token: String::new(),
            trading_symbol: String::new(),
            values: vec![None; 100],
            multiplier: 1.0,
            precision: 2,
        }
    }

    fn scale_fields(&self) -> (usize, usize) {
        match self.kind {
            TopicKind::Scrip => (23, 24),
            TopicKind::Depth => (32, 33),
            TopicKind::Index => (8, 9),
        }
    }

    fn set(&mut self, i: usize, v: u32) {
        if v != NOT_AVAILABLE && i < self.values.len() {
            self.values[i] = Some(v);
        }
    }

    fn set_scale(&mut self, updated: &[usize]) {
        let (m, p) = self.scale_fields();
        if updated.contains(&p) {
            if let Some(v) = self.values[p] {
                self.precision = v as i32;
            }
        }
        if updated.contains(&m) {
            if let Some(v) = self.values[m].filter(|v| *v != 0) {
                self.multiplier = f64::from(v);
            }
        }
    }

    /// A price field, scaled and rounded to the topic's precision.
    pub fn price(&self, i: usize) -> f64 {
        match self.values.get(i).copied().flatten() {
            Some(raw) => {
                let p = 10f64.powi(self.precision);
                let v = f64::from(raw) / (self.multiplier * p);
                (v * p).round() / p
            }
            None => 0.0,
        }
    }

    /// An integer field.
    pub fn long(&self, i: usize) -> i64 {
        self.values
            .get(i)
            .copied()
            .flatten()
            .map(i64::from)
            .unwrap_or(0)
    }
}

/// What one HSM frame says.
#[derive(Debug, Clone, PartialEq)]
pub enum HsmEvent {
    Connected {
        ok: bool,
    },
    Subscription {
        ok: bool,
    },
    /// Topic ids updated by this frame, in order.
    Data(Vec<u32>),
}

/// The HSM decoder: topic state plus the acknowledgement counter.
#[derive(Debug, Default)]
pub struct HsmDecoder {
    pub topics: HashMap<u32, Topic>,
    ack_every: u32,
    counter: u32,
    /// Message numbers due an acknowledgement; the feed sends them back as
    /// `FeedEvent::Reply` frames.
    pub pending_acks: Vec<u32>,
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    /// Big-endian unsigned of any width (web `buf2long`).
    fn uint(&mut self, n: usize) -> Option<u64> {
        self.take(n)
            .map(|s| s.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b)))
    }
}

impl HsmDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (a new connection).
    pub fn reset(&mut self) {
        self.topics.clear();
        self.ack_every = 0;
        self.counter = 0;
        self.pending_acks.clear();
    }

    fn status(c: &mut Cursor<'_>) -> Option<bool> {
        let n = c.u8()?;
        if n == 0 {
            return Some(false);
        }
        c.u8()?;
        let len = c.u16()? as usize;
        Some(c.take(len)? == b"K")
    }

    /// Decode one frame (web `HSWrapper.parseData`); malformed frames are
    /// dropped.
    pub fn decode(&mut self, frame: &[u8]) -> Option<HsmEvent> {
        if frame.len() < 3 {
            return None;
        }
        let mut c = Cursor { b: frame, pos: 2 };
        match c.u8()? {
            CONNECTION => {
                let n = c.u8()?;
                if n == 0 {
                    return Some(HsmEvent::Connected { ok: false });
                }
                c.u8()?;
                let len = c.u16()? as usize;
                let ok = c.take(len)? == b"K";
                if n >= 2 {
                    c.u8()?;
                    let len = c.u16()? as usize;
                    self.ack_every = c.uint(len)? as u32;
                }
                Some(HsmEvent::Connected { ok })
            }
            SUBSCRIBE | UNSUBSCRIBE => Some(HsmEvent::Subscription {
                ok: Self::status(&mut c)?,
            }),
            DATA => {
                if self.ack_every > 0 {
                    self.counter += 1;
                    let msg_num = c.u32()?;
                    if self.counter == self.ack_every {
                        self.counter = 0;
                        // Bounded: only the latest acknowledgement matters.
                        self.pending_acks.clear();
                        self.pending_acks.push(msg_num);
                    }
                }
                let count = c.u16()?;
                let mut updated = Vec::new();
                for _ in 0..count {
                    let sub_len = c.u16()? as usize;
                    let start = c.pos;
                    match c.u8()? {
                        SNAP => {
                            let id = c.u32()?;
                            let name_len = c.u8()? as usize;
                            let name = String::from_utf8_lossy(c.take(name_len)?).to_string();
                            let kind = match name.split('|').next() {
                                Some("sf") => Some(TopicKind::Scrip),
                                Some("if") => Some(TopicKind::Index),
                                Some("dp") => Some(TopicKind::Depth),
                                _ => None,
                            };
                            let n = c.u8()? as usize;
                            let mut vals = Vec::with_capacity(n);
                            for _ in 0..n {
                                vals.push(c.u32()?);
                            }
                            let m = c.u8()? as usize;
                            let mut strings = Vec::with_capacity(m);
                            for _ in 0..m {
                                let fid = c.u8()?;
                                let len = c.u8()? as usize;
                                strings
                                    .push((fid, String::from_utf8_lossy(c.take(len)?).to_string()));
                            }
                            let Some(kind) = kind else {
                                continue;
                            };
                            if self.topics.len() >= MAX_TOPICS && !self.topics.contains_key(&id) {
                                if let Some(k) = self.topics.keys().next().copied() {
                                    self.topics.remove(&k);
                                }
                            }
                            let mut t = Topic::new(kind);
                            for (i, v) in vals.iter().enumerate() {
                                t.set(i, *v);
                            }
                            let all: Vec<usize> = (0..vals.len()).collect();
                            t.set_scale(&all);
                            for (fid, s) in strings {
                                match fid {
                                    52 => t.token = s,
                                    53 => t.exchange = s,
                                    54 => t.trading_symbol = s,
                                    _ => {}
                                }
                            }
                            self.topics.insert(id, t);
                            updated.push(id);
                        }
                        UPDATE => {
                            let id = c.u32()?;
                            let n = c.u8()? as usize;
                            let mut vals = Vec::with_capacity(n);
                            for _ in 0..n {
                                vals.push(c.u32()?);
                            }
                            if let Some(t) = self.topics.get_mut(&id) {
                                let mut changed = Vec::new();
                                for (i, v) in vals.iter().enumerate() {
                                    if *v != NOT_AVAILABLE {
                                        changed.push(i);
                                    }
                                    t.set(i, *v);
                                }
                                t.set_scale(&changed);
                                updated.push(id);
                            }
                        }
                        _ => {
                            c.pos = start + sub_len;
                        }
                    }
                }
                Some(HsmEvent::Data(updated))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
    input: String,
    index: bool,
    aliases: Vec<(String, String)>,
}

/// The HSM market-data feed.
pub struct KotakHsmFeed {
    url: String,
    token: String,
    sid: String,
    decoder: HsmDecoder,
    subs: HashMap<String, SubInfo>,
    aliases: HashMap<(String, String), String>,
}

impl KotakHsmFeed {
    pub fn new(url: &str, token: &str, sid: &str) -> Self {
        Self {
            url: url.to_string(),
            token: token.to_string(),
            sid: sid.to_string(),
            decoder: HsmDecoder::new(),
            subs: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    fn key(s: &FeedSubscription) -> String {
        format!("{}:{}", s.exchange, s.symbol)
    }

    fn register(&mut self, s: &FeedSubscription, add: bool) -> Option<SubInfo> {
        let seg = kotak_segment(&s.exchange)?.to_string();
        let key = Self::key(s);
        let index = s.exchange.to_ascii_uppercase().contains("INDEX");
        let names = if index {
            index_candidates(&s.symbol)
        } else {
            Vec::new()
        };
        let input = if index {
            format!("{}|{}", seg, names.first().cloned().unwrap_or_default())
        } else {
            format!("{}|{}", seg, s.token.trim())
        };
        if !add {
            let old = self.subs.remove(&key)?;
            for a in &old.aliases {
                self.aliases.remove(a);
            }
            return Some(old);
        }
        let mut aliases = vec![(seg.clone(), s.token.trim().to_string())];
        for n in names {
            aliases.push((seg.clone(), n));
        }
        for a in &aliases {
            self.aliases.insert(a.clone(), key.clone());
        }
        let info = SubInfo {
            symbol: s.symbol.clone(),
            exchange: s.exchange.clone(),
            mode: s.mode,
            input,
            index,
            aliases,
        };
        self.subs.insert(key, info.clone());
        Some(info)
    }

    fn frames(&mut self, subs: &[FeedSubscription], add: bool) -> Vec<Message> {
        let mut sf = Vec::new();
        let mut dp = Vec::new();
        let mut idx = Vec::new();
        for s in subs {
            let Some(info) = self.register(s, add) else {
                continue;
            };
            if info.index {
                idx.push(info.input);
            } else {
                if info.mode == FeedMode::Depth {
                    dp.push(info.input.clone());
                }
                sf.push(info.input);
            }
        }
        let mut out = Vec::new();
        for (prefix, list) in [("sf", sf), ("dp", dp), ("if", idx)] {
            for chunk in list.chunks(MAX_SCRIPS) {
                if let Some(f) = subscription_frame(chunk, add, prefix, 1) {
                    out.push(Message::Binary(f));
                }
            }
        }
        out
    }

    fn sub_for(&self, t: &Topic) -> Option<&SubInfo> {
        let key = self
            .aliases
            .get(&(t.exchange.clone(), t.token.clone()))
            .or_else(|| {
                self.aliases
                    .get(&(t.exchange.clone(), t.trading_symbol.clone()))
            })?;
        self.subs.get(key)
    }

    fn events(&self, ids: &[u32]) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        let now = now_ms();
        for id in ids {
            let Some(t) = self.decoder.topics.get(id) else {
                continue;
            };
            let Some(sub) = self.sub_for(t) else {
                continue;
            };
            match t.kind {
                TopicKind::Scrip | TopicKind::Index => {
                    let (ltp, open, high, low, close) = if t.kind == TopicKind::Index {
                        (t.price(2), t.price(7), t.price(5), t.price(6), t.price(3))
                    } else {
                        (
                            t.price(5),
                            t.price(20),
                            t.price(15),
                            t.price(14),
                            t.price(21),
                        )
                    };
                    if ltp <= 0.0 {
                        continue;
                    }
                    let mut tick = NormalizedTick {
                        symbol: sub.symbol.clone(),
                        exchange: sub.exchange.clone(),
                        mode: sub.mode.code(),
                        ltp,
                        timestamp_ms: now,
                        ..Default::default()
                    };
                    if sub.mode != FeedMode::Ltp {
                        tick.open = open;
                        tick.high = high;
                        tick.low = low;
                        tick.close = close;
                        if t.kind == TopicKind::Scrip {
                            tick.volume = t.long(4);
                            tick.last_quantity = t.long(6);
                            tick.total_buy_quantity = t.long(7);
                            tick.total_sell_quantity = t.long(8);
                            tick.average_price = t.price(13);
                            tick.oi = t.long(22);
                        }
                    }
                    tick.derive_change();
                    out.push(FeedEvent::Tick(tick));
                }
                TopicKind::Depth => {
                    if sub.mode != FeedMode::Depth {
                        continue;
                    }
                    let side = |p0: usize, q0: usize, o0: usize| -> Vec<DepthLevel> {
                        (0..5)
                            .map(|i| DepthLevel {
                                price: t.price(p0 + i),
                                quantity: t.long(q0 + i),
                                orders: t.long(o0 + i),
                            })
                            .collect()
                    };
                    let buy = side(2, 12, 22);
                    let sell = side(7, 17, 27);
                    out.push(FeedEvent::Depth(NormalizedDepth {
                        symbol: sub.symbol.clone(),
                        exchange: sub.exchange.clone(),
                        ltp: 0.0,
                        total_buy_quantity: buy.iter().map(|l| l.quantity).sum(),
                        total_sell_quantity: sell.iter().map(|l| l.quantity).sum(),
                        buy,
                        sell,
                        timestamp_ms: now,
                    }));
                }
            }
        }
        out
    }

    /// Topics remembered (bounded by `MAX_TOPICS`).
    pub fn topic_count(&self) -> usize {
        self.decoder.topics.len()
    }
}

impl BrokerFeed for KotakHsmFeed {
    fn broker(&self) -> &'static str {
        "kotak"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Kotak HSM feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.decoder.reset();
        vec![Message::Binary(connect_frame(&self.token, &self.sid))]
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, true)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, false)
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        let Message::Binary(b) = msg else {
            return Vec::new();
        };
        match self.decoder.decode(b) {
            Some(HsmEvent::Connected { ok: true }) => vec![FeedEvent::AuthOk],
            Some(HsmEvent::Connected { ok: false }) => vec![FeedEvent::AuthFailed(
                "Kotak refused the live market data session. Log in to Kotak again.".into(),
            )],
            Some(HsmEvent::Data(ids)) => {
                let mut out = self.events(&ids);
                out.extend(
                    self.decoder
                        .pending_acks
                        .drain(..)
                        .map(|n| FeedEvent::Reply(Message::Binary(ack_frame(n)))),
                );
                out
            }
            _ => Vec::new(),
        }
    }
}
