//! NxtradStream binary feed (web `streaming/nxtradstream.py`,
//! `streaming/tradejini_adapter.py`, `streaming/tradejini_mapping.py`).
//!
//! URL: `wss://api.tradejini.com/v2.1/stream?token=<api_key>:<access_token>&version=3.1`;
//! authentication is in the URL and the server answers with packet type 13
//! (`auth_status`, 1 = ok).
//!
//! Requests are JSON text followed by `"\n"`:
//! `{"type":"L1"|"L5","action":"sub","tokens":[{"t":"<token>_<EXCH>"}]}`.
//! A `sub` REPLACES the server-side list of that feed and `unsub` clears the
//! whole feed, so every change re-sends the complete list for the feed.
//!
//! Frame layout (little-endian, `nxtradstream.py:613-634`):
//!
//! | offset | type  | field                                   |
//! |--------|-------|-----------------------------------------|
//! | 0..4   | i32   | total length                            |
//! | 4      | i8    | version, must be 1                      |
//! | 5      | i8    | compression, 100 = zlib on `[6..]`      |
//! | 6..    |       | packets                                 |
//!
//! Each packet: `[0..2]` i16 packet length (including itself), `[2]` i8
//! packet type (10 L1, 11 L5, 12 OHLC, 13 auth, 14 market status, 15 events,
//! 16 ping, 17 greeks), then from offset 3 repeated `u8 field key` + value
//! whose width comes from the per-type spec (`DEFAULT_PKT_INFO`, lines
//! 63-160). Prices are integers divided by the segment divisor (`SEG_INFO`:
//! NSE/BSE/NFO/BFO/MCX 100, CDS 1e7, BCD/MCD/NCO/BCO 1e4); `chngPer` and
//! `OIChngPer` are always divided by 100. L1 packets are deltas merged into a
//! per-symbol cache keyed `"<token>_<EXCH>"`.

use crate::brokers::common::streaming::{
    now_ms, round2, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::secret::Secret;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const STREAM_URL: &str = "wss://api.tradejini.com/v2.1/stream";
pub const STREAM_VERSION: &str = "3.1";
/// Client keepalive (`NxtradStream.sendPing`); the server answers with
/// packet type 16, which keeps the manager's stall watchdog fed on a quiet
/// feed.
pub const HEARTBEAT: Duration = Duration::from_secs(30);
/// A decompressed frame larger than this is dropped (a full L5 batch of
/// 3000 instruments is well under 1 MB).
const MAX_INFLATED: u64 = 8 * 1024 * 1024;

/// `SEG_INFO`: segment id -> (exchange, divisor).
pub fn segment(id: u8) -> Option<(&'static str, f64)> {
    Some(match id {
        1 => ("NSE", 100.0),
        2 => ("BSE", 100.0),
        3 => ("NFO", 100.0),
        4 => ("BFO", 100.0),
        5 => ("CDS", 10_000_000.0),
        6 => ("BCD", 10_000.0),
        7 => ("MCD", 10_000.0),
        8 => ("MCX", 100.0),
        9 => ("NCO", 10_000.0),
        10 => ("BCO", 10_000.0),
        _ => return None,
    })
}

/// Merged L1 state of one instrument. `None` means the field never arrived.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct L1 {
    pub token: i64,
    pub exch: String,
    pub ltp: Option<f64>,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    pub chng: Option<f64>,
    pub chng_per: Option<f64>,
    pub atp: Option<f64>,
    pub ltq: Option<i64>,
    pub vol: Option<i64>,
    pub oi: Option<i64>,
    pub ucl: Option<f64>,
    pub lcl: Option<f64>,
    /// Last trade time, epoch seconds.
    pub ltt: Option<i64>,
    pub bid_price: Option<f64>,
    pub ask_price: Option<f64>,
    pub bid_qty: Option<i64>,
    pub ask_qty: Option<i64>,
}

impl L1 {
    /// `"<token>_<EXCH>"`, the web's `symbol` key.
    pub fn key(&self) -> String {
        format!("{}_{}", self.token, self.exch)
    }

    /// Overlay a delta (web `L1_dict[t].update(jData)`).
    pub fn merge(&mut self, d: &L1) {
        macro_rules! take {
            ($($f:ident),*) => { $( if d.$f.is_some() { self.$f = d.$f; } )* };
        }
        take!(
            ltp, open, high, low, close, chng, chng_per, atp, ltq, vol, oi, ucl, lcl, ltt,
            bid_price, ask_price, bid_qty, ask_qty
        );
    }

    /// web `_l1_complete`: every quote field present, plus OI on a
    /// derivatives exchange.
    pub fn is_complete(&self, oa_exchange: &str) -> bool {
        let base = self.ltp.is_some()
            && self.open.is_some()
            && self.high.is_some()
            && self.low.is_some()
            && self.close.is_some()
            && self.vol.is_some()
            && self.bid_price.is_some()
            && self.ask_price.is_some();
        let needs_oi = matches!(oa_exchange, "NFO" | "BFO" | "CDS" | "BCD" | "MCX");
        base && (!needs_oi || self.oi.is_some())
    }
}

/// One L5 book.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct L5 {
    pub token: i64,
    pub exch: String,
    pub tot_buy_qty: i64,
    pub tot_sell_qty: i64,
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
}

impl L5 {
    pub fn key(&self) -> String {
        format!("{}_{}", self.token, self.exch)
    }
}

/// A decoded packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    /// Boxed: an L1 packet is several times the size of the others.
    L1(Box<L1>),
    L5(L5),
    /// Packet 13, field 25.
    Auth(u8),
    /// Packet 16.
    Pong,
    /// OHLC, market status, events, greeks: not used by OpenAlgo.
    Other(i8),
}

/// Width of a field value, per packet type (`DEFAULT_PKT_INFO`).
fn width(pkt: i8, key: u8) -> Option<usize> {
    let w = match (pkt, key) {
        (10, 26 | 28 | 55) => 1,
        (10, 56) => 2,
        (10, 41) => 8,
        (10, 27 | 29..=40 | 42..=46 | 49..=54 | 58..=60 | 70 | 71 | 74) => 4,
        (11, 26 | 28 | 55) => 1,
        (11, 27 | 47..=54) => 4,
        (13, 25) => 1,
        (16, 62) => 1,
        _ => return None,
    };
    Some(w)
}

fn rd_i32(b: &[u8]) -> i64 {
    i64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn rd_u32(b: &[u8]) -> i64 {
    i64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Walk `key, value` pairs from offset 3; stops at an unknown key or a
/// value that would run past the packet (the web raises and drops the rest
/// of the frame there).
fn fields(pkt_type: i8, p: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut idx = 3;
    while idx < p.len() {
        let key = p[idx];
        idx += 1;
        let Some(w) = width(pkt_type, key) else {
            tracing::debug!(
                "Tradejini feed: unknown field {} in packet {}",
                key,
                pkt_type
            );
            break;
        };
        if idx + w > p.len() {
            break;
        }
        out.push((key, &p[idx..idx + w]));
        idx += w;
    }
    out
}

/// Decode an L1 packet (`__decodeL1PKT`). Prices are divided after the
/// whole packet is read, because `exchSeg` may follow them.
pub fn decode_l1(p: &[u8]) -> Option<L1> {
    let mut seg: Option<(&str, f64)> = None;
    let mut token: Option<i64> = None;
    let mut raw: Vec<(u8, &[u8])> = Vec::new();
    for (k, v) in fields(10, p) {
        match k {
            26 => seg = segment(v[0]),
            27 => token = Some(rd_i32(v)),
            _ => raw.push((k, v)),
        }
    }
    let (exch, div) = seg?;
    let mut l = L1 {
        token: token?,
        exch: exch.to_string(),
        ..Default::default()
    };
    for (k, v) in raw {
        let price = || Some(rd_i32(v) as f64 / div);
        match k {
            29 => l.ltp = price(),
            30 => l.open = price(),
            31 => l.high = price(),
            32 => l.low = price(),
            33 => l.close = price(),
            34 => l.chng = price(),
            35 => l.chng_per = Some(rd_i32(v) as f64 / 100.0),
            36 => l.atp = price(),
            39 => l.ltq = Some(rd_u32(v)),
            40 => l.vol = Some(rd_u32(v)),
            42 => l.ucl = price(),
            43 => l.lcl = price(),
            44 => l.oi = Some(rd_u32(v)),
            46 => l.ltt = Some(rd_i32(v)),
            49 => l.bid_price = price(),
            50 => l.bid_qty = Some(rd_u32(v)),
            52 => l.ask_price = price(),
            53 => l.ask_qty = Some(rd_u32(v)),
            _ => {}
        }
    }
    Some(l)
}

/// Decode an L5 packet (`__decodeL2PKT`): after `nDepth` (55), every three
/// fields form a level `{price, qty, no}`; the first `nDepth` levels are
/// bids, the rest asks. Level prices use the divisor known at that point.
pub fn decode_l5(p: &[u8]) -> Option<L5> {
    let mut div = 100.0;
    let mut seg: Option<&str> = None;
    let mut token: Option<i64> = None;
    let mut n_levels: Option<usize> = None;
    let mut bids: Vec<DepthLevel> = Vec::new();
    let mut asks: Vec<DepthLevel> = Vec::new();
    let mut cur: Vec<(u8, &[u8])> = Vec::new();
    let mut tot_raw: Vec<(u8, &[u8])> = Vec::new();
    for (k, v) in fields(11, p) {
        match k {
            55 => n_levels = Some(usize::from(v[0])),
            26 => {
                if let Some((e, d)) = segment(v[0]) {
                    seg = Some(e);
                    div = d;
                }
            }
            27 if n_levels.is_none() => token = Some(rd_i32(v)),
            _ if n_levels.is_some() => {
                cur.push((k, v));
                if cur.len() == 3 {
                    let mut lvl = DepthLevel::default();
                    for (fk, fv) in cur.drain(..) {
                        match fk {
                            49 | 52 => lvl.price = rd_i32(fv) as f64 / div,
                            50 | 53 => lvl.quantity = rd_u32(fv),
                            51 | 54 => lvl.orders = rd_u32(fv),
                            _ => {}
                        }
                    }
                    if bids.len() < n_levels.unwrap_or(0) {
                        bids.push(lvl);
                    } else {
                        asks.push(lvl);
                    }
                }
            }
            _ => tot_raw.push((k, v)),
        }
    }
    let mut book = L5 {
        token: token?,
        exch: seg?.to_string(),
        bids,
        asks,
        ..Default::default()
    };
    for (k, v) in tot_raw {
        match k {
            47 => book.tot_buy_qty = rd_u32(v),
            48 => book.tot_sell_qty = rd_u32(v),
            _ => {}
        }
    }
    Some(book)
}

/// Decode one packet slice (`__onsinglePacket`).
pub fn decode_packet(p: &[u8]) -> Option<Packet> {
    if p.len() < 3 {
        return None;
    }
    let t = p[2] as i8;
    match t {
        10 => decode_l1(p).map(|l| Packet::L1(Box::new(l))),
        11 => decode_l5(p).map(Packet::L5),
        13 => fields(13, p)
            .into_iter()
            .find(|(k, _)| *k == 25)
            .map(|(_, v)| Packet::Auth(v[0])),
        16 => Some(Packet::Pong),
        12 | 14 | 15 | 17 => Some(Packet::Other(t)),
        _ => None,
    }
}

/// Decode a whole binary message (`__on_message`): header, optional zlib,
/// then length-prefixed packets.
pub fn decode_message(m: &[u8]) -> Vec<Packet> {
    if m.len() < 6 {
        return Vec::new();
    }
    if m[4] as i8 != 1 {
        tracing::debug!("Tradejini feed: unsupported frame version {}", m[4]);
        return Vec::new();
    }
    let body: Vec<u8> = if m[5] as i8 == 100 {
        let mut out = Vec::new();
        let dec = flate2::read::ZlibDecoder::new(&m[6..]);
        if dec.take(MAX_INFLATED).read_to_end(&mut out).is_err() {
            tracing::debug!("Tradejini feed: frame could not be inflated");
            return Vec::new();
        }
        out
    } else {
        m[6..].to_vec()
    };
    let mut out = Vec::new();
    let mut idx = 0usize;
    while idx + 2 <= body.len() {
        let len = i16::from_le_bytes([body[idx], body[idx + 1]]);
        if len <= 0 {
            break;
        }
        let end = (idx + len as usize).min(body.len());
        if let Some(p) = decode_packet(&body[idx..end]) {
            out.push(p);
        }
        idx += len as usize;
    }
    out
}

/// One subscription request frame (`subscribeL1` / `subscribeL2`).
pub fn sub_frame(feed: &str, keys: &[String]) -> Message {
    let tokens: Vec<_> = keys.iter().map(|k| json!({ "t": k })).collect();
    Message::Text(format!(
        "{}\n",
        json!({"type": feed, "action": "sub", "tokens": tokens})
    ))
}

/// `unsubscribeL1` / `unsubscribeL2`: clears the whole feed.
pub fn unsub_frame(feed: &str) -> Message {
    Message::Text(format!("{}\n", json!({"type": feed, "action": "unsub"})))
}

/// `"<token>_<EXCH>"` for a subscription; the web strips `_INDEX`.
pub fn ws_key(token: &str, brexchange: &str, exchange: &str) -> String {
    let e = if brexchange.is_empty() {
        exchange.replace("_INDEX", "")
    } else {
        brexchange.replace("_INDEX", "")
    };
    format!("{}_{}", token, e)
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

/// The live market-data feed.
pub struct TradejiniFeed {
    /// Carries the access token in its query.
    url: Secret,
    /// `ws_key` -> subscription (ordered so frames are deterministic).
    subs: BTreeMap<String, SubInfo>,
    l1: HashMap<String, L1>,
    l5_totals: HashMap<String, (i64, i64)>,
}

impl TradejiniFeed {
    /// `base` is the stream endpoint (`STREAM_URL`; tests pass a local one).
    pub fn new(base: &str, api_key: &str, access_token: &str) -> Self {
        Self {
            url: Secret::new(format!(
                "{}?token={}:{}&version={}",
                base, api_key, access_token, STREAM_VERSION
            )),
            subs: BTreeMap::new(),
            l1: HashMap::new(),
            l5_totals: HashMap::new(),
        }
    }

    /// Cached instruments (bounded by the subscriptions).
    pub fn cached(&self) -> usize {
        self.l1.len() + self.l5_totals.len()
    }

    /// Every instrument rides L1 (ticks for LTP, Quote and Depth clients);
    /// depth instruments also ride L5.
    fn lists(&self) -> (Vec<String>, Vec<String>) {
        let l1: Vec<String> = self.subs.keys().cloned().collect();
        let l5: Vec<String> = self
            .subs
            .iter()
            .filter(|(_, s)| s.mode == FeedMode::Depth)
            .map(|(k, _)| k.clone())
            .collect();
        (l1, l5)
    }

    fn sync(&self, l1_changed: bool, l5_changed: bool) -> Vec<Message> {
        let (l1, l5) = self.lists();
        let mut out = Vec::new();
        if l1_changed {
            out.push(if l1.is_empty() {
                unsub_frame("L1")
            } else {
                sub_frame("L1", &l1)
            });
        }
        if l5_changed {
            out.push(if l5.is_empty() {
                unsub_frame("L5")
            } else {
                sub_frame("L5", &l5)
            });
        }
        out
    }

    fn tick(&self, sub: &SubInfo, q: &L1) -> NormalizedTick {
        let (tbq, tsq) = self.l5_totals.get(&q.key()).copied().unwrap_or((0, 0));
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: q.ltp.unwrap_or(0.0),
            open: q.open.unwrap_or(0.0),
            high: q.high.unwrap_or(0.0),
            low: q.low.unwrap_or(0.0),
            close: q.close.unwrap_or(0.0),
            volume: q.vol.unwrap_or(0),
            average_price: q.atp.unwrap_or(0.0),
            last_quantity: q.ltq.unwrap_or(0),
            total_buy_quantity: tbq,
            total_sell_quantity: tsq,
            oi: q.oi.unwrap_or(0),
            change: q.chng.unwrap_or(0.0),
            change_percent: q.chng_per.unwrap_or(0.0),
            last_trade_time_ms: q.ltt.unwrap_or(0) * 1000,
            timestamp_ms: now_ms(),
        };
        if q.chng.is_none() {
            t.derive_change();
        } else {
            t.change = round2(t.change);
        }
        t
    }

    /// Decode one message into events.
    pub fn parse_binary(&mut self, m: &[u8]) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        for p in decode_message(m) {
            match p {
                Packet::Auth(1) => out.push(FeedEvent::AuthOk),
                Packet::Auth(code) => {
                    tracing::warn!("Tradejini feed refused the session (status {})", code);
                    out.push(FeedEvent::AuthFailed(
                        "Tradejini refused the live market data connection. Log in to Tradejini again."
                            .into(),
                    ));
                }
                Packet::Pong => out.push(FeedEvent::Heartbeat),
                Packet::L1(d) => {
                    let key = d.key();
                    let Some(sub) = self.subs.get(&key).cloned() else {
                        continue;
                    };
                    let entry = self.l1.entry(key).or_insert_with(|| L1 {
                        token: d.token,
                        exch: d.exch.clone(),
                        ..Default::default()
                    });
                    entry.merge(&d);
                    let snapshot = entry.clone();
                    out.push(FeedEvent::Tick(self.tick(&sub, &snapshot)));
                }
                Packet::L5(book) => {
                    let key = book.key();
                    let Some(sub) = self.subs.get(&key).cloned() else {
                        continue;
                    };
                    self.l5_totals
                        .insert(key.clone(), (book.tot_buy_qty, book.tot_sell_qty));
                    if sub.mode != FeedMode::Depth {
                        continue;
                    }
                    let ltp = self.l1.get(&key).and_then(|q| q.ltp).unwrap_or(0.0);
                    out.push(FeedEvent::Depth(NormalizedDepth {
                        symbol: sub.symbol,
                        exchange: sub.exchange,
                        ltp,
                        buy: pad5(&book.bids),
                        sell: pad5(&book.asks),
                        total_buy_quantity: book.tot_buy_qty,
                        total_sell_quantity: book.tot_sell_qty,
                        timestamp_ms: now_ms(),
                    }));
                }
                Packet::Other(_) => {}
            }
        }
        out
    }
}

/// Five levels, padded with empty ones (web `_extract_depth_data`).
pub fn pad5(levels: &[DepthLevel]) -> Vec<DepthLevel> {
    let mut v: Vec<DepthLevel> = levels.iter().take(5).copied().collect();
    while v.len() < 5 {
        v.push(DepthLevel::default());
    }
    v
}

impl BrokerFeed for TradejiniFeed {
    fn broker(&self) -> &'static str {
        "tradejini"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .expose()
            .into_client_request()
            .map_err(|_| AppError::Internal("Market data feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // Subscriptions do not survive a reconnect; the manager replays them
        // through `subscribe_frames`, which re-sends each feed's full list.
        self.l1.clear();
        self.l5_totals.clear();
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut l5_changed = false;
        for s in subs {
            let k = ws_key(&s.token, &s.brexchange, &s.exchange);
            l5_changed |= s.mode == FeedMode::Depth;
            self.subs.insert(
                k,
                SubInfo {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    mode: s.mode,
                },
            );
        }
        if subs.is_empty() {
            return Vec::new();
        }
        self.sync(true, l5_changed)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut l5_changed = false;
        for s in subs {
            let k = ws_key(&s.token, &s.brexchange, &s.exchange);
            if let Some(old) = self.subs.remove(&k) {
                l5_changed |= old.mode == FeedMode::Depth;
            }
            self.l1.remove(&k);
            self.l5_totals.remove(&k);
        }
        if subs.is_empty() {
            return Vec::new();
        }
        self.sync(true, l5_changed)
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        let k = ws_key(&new.token, &new.brexchange, &new.exchange);
        let l5_changed = (old.mode == FeedMode::Depth) != (new.mode == FeedMode::Depth);
        self.subs.insert(
            k,
            SubInfo {
                symbol: new.symbol.clone(),
                exchange: new.exchange.clone(),
                mode: new.mode,
            },
        );
        // Both modes ride L1, so only an L5 change needs a frame.
        self.sync(false, l5_changed)
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((
            HEARTBEAT,
            Message::Text(format!("{}\n", json!({"type": "PING"}))),
        ))
    }
}

// ---------------------------------------------------------------------------
// Frame builders (tests and the local fake broker)
// ---------------------------------------------------------------------------

/// Packet builder: `pktLen i16, pktType i8, (key u8, value)*`.
pub mod build {
    /// A field value.
    pub enum V {
        U8(u8),
        I32(i32),
        U32(u32),
    }

    pub fn packet(pkt_type: i8, fields: &[(u8, V)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (k, v) in fields {
            body.push(*k);
            match v {
                V::U8(x) => body.push(*x),
                V::I32(x) => body.extend_from_slice(&x.to_le_bytes()),
                V::U32(x) => body.extend_from_slice(&x.to_le_bytes()),
            }
        }
        let len = (body.len() + 3) as i16;
        let mut p = len.to_le_bytes().to_vec();
        p.push(pkt_type as u8);
        p.extend(body);
        p
    }

    /// Frame header `i32 len, i8 version=1, i8 compression` + packets;
    /// `zlib` compresses the packet bytes with compression code 100.
    pub fn frame(packets: &[Vec<u8>], zlib: bool) -> Vec<u8> {
        use std::io::Write;
        let raw: Vec<u8> = packets.concat();
        let body = if zlib {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            let _ = e.write_all(&raw);
            e.finish().unwrap_or_default()
        } else {
            raw
        };
        let mut f = ((body.len() + 6) as i32).to_le_bytes().to_vec();
        f.push(1);
        f.push(if zlib { 100 } else { 0 });
        f.extend(body);
        f
    }

    /// An L1 packet with every quote field (prices in rupees x divisor).
    #[allow(clippy::too_many_arguments)]
    pub fn l1_full(
        seg: u8,
        token: i32,
        ltp: i32,
        open: i32,
        high: i32,
        low: i32,
        close: i32,
        vol: u32,
        bid: i32,
        ask: i32,
        oi: Option<u32>,
    ) -> Vec<u8> {
        let mut f = vec![
            (29, V::I32(ltp)),
            (30, V::I32(open)),
            (31, V::I32(high)),
            (32, V::I32(low)),
            (33, V::I32(close)),
            (34, V::I32(ltp - close)),
            (
                35,
                V::I32(if close != 0 {
                    (i64::from(ltp - close) * 10_000 / i64::from(close)) as i32
                } else {
                    0
                }),
            ),
            (36, V::I32(ltp)),
            (39, V::U32(5)),
            (40, V::U32(vol)),
            (46, V::I32(1_759_475_100)),
            (49, V::I32(bid)),
            (50, V::U32(100)),
            (52, V::I32(ask)),
            (53, V::U32(200)),
        ];
        if let Some(o) = oi {
            f.push((44, V::U32(o)));
        }
        // exchSeg and token last: the decoder must not depend on order.
        f.push((26, V::U8(seg)));
        f.push((27, V::I32(token)));
        packet(10, &f)
    }

    /// An L5 packet: totals, then `nDepth`, then bid and ask levels.
    pub fn l5(seg: u8, token: i32, bids: &[(i32, u32, u32)], asks: &[(i32, u32, u32)]) -> Vec<u8> {
        let mut f = vec![
            (26, V::U8(seg)),
            (27, V::I32(token)),
            (47, V::U32(bids.iter().map(|b| b.1).sum())),
            (48, V::U32(asks.iter().map(|a| a.1).sum())),
            (55, V::U8(bids.len() as u8)),
        ];
        for (p, q, n) in bids {
            f.push((49, V::I32(*p)));
            f.push((50, V::U32(*q)));
            f.push((51, V::U32(*n)));
        }
        for (p, q, n) in asks {
            f.push((52, V::I32(*p)));
            f.push((53, V::U32(*q)));
            f.push((54, V::U32(*n)));
        }
        packet(11, &f)
    }

    /// Auth acknowledgement (packet 13, field 25).
    pub fn auth(status: u8) -> Vec<u8> {
        packet(13, &[(25, V::U8(status))])
    }
}
