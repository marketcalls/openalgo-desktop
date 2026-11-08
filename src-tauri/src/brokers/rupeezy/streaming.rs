//! The Vortex price feed (web `streaming/rupeezy_websocket.py`,
//! `rupeezy_adapter.py`, `rupeezy_mapping.py`).
//!
//! * `wss://wire.rupeezy.in/ws?auth_token=<token>`. The shared feed manager
//!   owns the socket, the reconnects and the re-subscription; the client
//!   pings every 10 s, as the official SDK keeps the link alive.
//! * Subscribe and unsubscribe are JSON text frames, one per instrument:
//!   `{"ticker": "NSE:SBIN", "mode": "full", "message_type": "subscribe"}`.
//!   Subscribing a ticker again replaces its mode.
//! * OpenAlgo modes 1/2/3 are Vortex `ltp` / `ohlcv` / `full`.
//! * Price frames are binary and little-endian: a u16 packet count, then
//!   per packet a u16 length and the packet. The length names the mode: 22
//!   `ltp`, 62 `ohlcv`, 266 `full` (5-level depth). Every packet starts
//!   with a 10-byte exchange (`NSE_EQ`) and an i32 token. A frame under two
//!   bytes is a heartbeat.
//! * Text frames carry order postbacks, which the web does not use either.
//! * At most 1000 instruments per connection.

use super::mapping::ticker;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Instruments one Vortex connection carries. Past it the web answers the
/// client `SUBSCRIBE_ERROR`; here the feed bridge subscribes the broker
/// after the client's subscribe is acknowledged, and a broker refusal can
/// only be logged (as the manager's own cap is), so the extra instrument
/// is left out with a warning.
pub const MAX_INSTRUMENTS: usize = 1000;
pub const PING_INTERVAL: Duration = Duration::from_secs(10);

pub const LTP_SIZE: usize = 22;
pub const OHLCV_SIZE: usize = 62;
pub const FULL_SIZE: usize = 266;
/// The full packet's fixed part, before its ten 16-byte depth levels.
const FULL_HEAD: usize = 98;
const LEVEL_SIZE: usize = 16;

/// OpenAlgo mode -> Vortex mode.
pub fn vortex_mode(mode: FeedMode) -> &'static str {
    match mode {
        FeedMode::Ltp => "ltp",
        FeedMode::Quote => "ohlcv",
        FeedMode::Depth => "full",
    }
}

fn i32_at(b: &[u8], o: usize) -> i32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[o..o + 4]);
    i32::from_le_bytes(a)
}

fn i64_at(b: &[u8], o: usize) -> i64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    i64::from_le_bytes(a)
}

fn f64_at(b: &[u8], o: usize) -> f64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    f64::from_le_bytes(a)
}

/// One decoded price packet; fields its mode does not carry are zero.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VortexPacket {
    pub exchange: String,
    pub token: i32,
    pub ltp: f64,
    /// Last trade time, epoch seconds.
    pub ltt: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    /// Previous close.
    pub close: f64,
    pub volume: i64,
    /// Epoch seconds.
    pub last_update_time: i64,
    pub ltq: i64,
    pub average_price: f64,
    pub total_buy_quantity: i64,
    pub total_sell_quantity: i64,
    pub oi: i64,
    /// Five bids and five asks (full packets only).
    pub depth: Option<(Vec<DepthLevel>, Vec<DepthLevel>)>,
    /// Price band, from paise.
    pub upper_limit: f64,
    pub lower_limit: f64,
    /// Carries open, high, low, close and volume (`ohlcv` and `full`).
    pub has_ohlcv: bool,
}

/// One packet by its length (web `parse_packet`); `None` for a length no
/// mode has.
pub fn decode_packet(p: &[u8]) -> Option<VortexPacket> {
    let n = p.len();
    if n != LTP_SIZE && n != OHLCV_SIZE && n != FULL_SIZE {
        return None;
    }
    let exchange = String::from_utf8_lossy(&p[..10])
        .trim_end_matches('\0')
        .to_string();
    let mut out = VortexPacket {
        exchange,
        token: i32_at(p, 10),
        ltp: f64_at(p, 14),
        ..Default::default()
    };
    if n == LTP_SIZE {
        return Some(out);
    }
    out.has_ohlcv = true;
    out.ltt = i64::from(i32_at(p, 22));
    out.open = f64_at(p, 26);
    out.high = f64_at(p, 34);
    out.low = f64_at(p, 42);
    out.close = f64_at(p, 50);
    out.volume = i64::from(i32_at(p, 58));
    if n == OHLCV_SIZE {
        return Some(out);
    }
    out.last_update_time = i64::from(i32_at(p, 62));
    out.ltq = i64::from(i32_at(p, 66));
    out.average_price = f64_at(p, 70);
    out.total_buy_quantity = i64_at(p, 78);
    out.total_sell_quantity = i64_at(p, 86);
    out.oi = i64::from(i32_at(p, 94));
    let level = |i: usize| {
        let o = FULL_HEAD + i * LEVEL_SIZE;
        DepthLevel {
            price: f64_at(p, o),
            quantity: i64::from(i32_at(p, o + 8)),
            orders: i64::from(i32_at(p, o + 12)),
        }
    };
    out.depth = Some(((0..5).map(level).collect(), (5..10).map(level).collect()));
    out.upper_limit = f64::from(i32_at(p, n - 8)) / 100.0;
    out.lower_limit = f64::from(i32_at(p, n - 4)) / 100.0;
    Some(out)
}

fn u16_at(b: &[u8], o: usize) -> usize {
    usize::from(u16::from_le_bytes([b[o], b[o + 1]]))
}

/// A binary frame's packets (web `split_message`); a short last packet is
/// passed on as it is and refused by [`decode_packet`].
pub fn split_frame(frame: &[u8]) -> Vec<&[u8]> {
    if frame.len() < 2 {
        return Vec::new();
    }
    let count = u16_at(frame, 0);
    let mut packets = Vec::with_capacity(count.min(1024));
    let mut offset = 2;
    for _ in 0..count {
        if offset + 2 > frame.len() {
            break;
        }
        let len = u16_at(frame, offset);
        offset += 2;
        let end = (offset + len).min(frame.len());
        packets.push(&frame[offset..end]);
        offset += len;
    }
    packets
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    ticker: String,
    mode: FeedMode,
}

pub struct RupeezyFeed {
    url: String,
    /// `(Vortex exchange, token)` -> the subscription its packets serve.
    subs: HashMap<(String, i32), SubInfo>,
}

impl RupeezyFeed {
    pub fn new(base: &str, access_token: &str) -> Self {
        Self {
            url: format!("{}?auth_token={}", base, urlencoding::encode(access_token)),
            subs: HashMap::new(),
        }
    }

    fn key(s: &FeedSubscription) -> Option<(String, i32)> {
        let token = s.token.trim().parse::<i32>().ok();
        if token.is_none() {
            tracing::warn!("No Rupeezy token for {}:{}", s.exchange, s.symbol);
        }
        Some((s.brexchange.trim().to_ascii_uppercase(), token?))
    }

    fn frame(ticker: &str, mode: FeedMode, kind: &str) -> Message {
        Message::Text(
            json!({"ticker": ticker, "mode": vortex_mode(mode), "message_type": kind}).to_string(),
        )
    }

    /// Record one subscription and its subscribe frame; refused (no frame)
    /// past the per-connection limit.
    fn subscribe_one(&mut self, s: &FeedSubscription) -> Option<Message> {
        let key = Self::key(s)?;
        if !self.subs.contains_key(&key) && self.subs.len() >= MAX_INSTRUMENTS {
            tracing::warn!(
                "Rupeezy streams at most {} instruments; {}:{} was not subscribed",
                MAX_INSTRUMENTS,
                s.exchange,
                s.symbol
            );
            return None;
        }
        let info = SubInfo {
            symbol: s.symbol.clone(),
            exchange: s.exchange.clone(),
            ticker: ticker(&s.brexchange, &s.brsymbol),
            mode: s.mode,
        };
        let frame = Self::frame(&info.ticker, info.mode, "subscribe");
        self.subs.insert(key, info);
        Some(frame)
    }

    /// The events of one packet, under its subscription's OpenAlgo symbol.
    fn packet_events(&self, packet: &[u8]) -> Vec<FeedEvent> {
        let Some(p) = decode_packet(packet) else {
            return Vec::new();
        };
        let Some(sub) = self.subs.get(&(p.exchange.clone(), p.token)) else {
            return Vec::new();
        };
        let ts = match p.last_update_time.max(0) {
            0 if p.ltt > 0 => p.ltt * 1000,
            0 => now_ms(),
            t => t * 1000,
        };
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: p.ltp,
            close: p.close,
            last_trade_time_ms: p.ltt * 1000,
            timestamp_ms: ts,
            ..Default::default()
        };
        if p.has_ohlcv {
            t.open = p.open;
            t.high = p.high;
            t.low = p.low;
            t.volume = p.volume;
        }
        t.average_price = p.average_price;
        t.last_quantity = p.ltq;
        t.total_buy_quantity = p.total_buy_quantity;
        t.total_sell_quantity = p.total_sell_quantity;
        t.oi = p.oi;
        t.derive_change();
        let depth = p.depth.map(|(buy, sell)| NormalizedDepth {
            symbol: t.symbol.clone(),
            exchange: t.exchange.clone(),
            ltp: t.ltp,
            buy,
            sell,
            total_buy_quantity: t.total_buy_quantity,
            total_sell_quantity: t.total_sell_quantity,
            timestamp_ms: ts,
            ..Default::default()
        });
        let mut out = vec![FeedEvent::Tick(t)];
        out.extend(depth.map(FeedEvent::Depth));
        out
    }
}

impl BrokerFeed for RupeezyFeed {
    fn broker(&self) -> &'static str {
        "rupeezy"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Rupeezy stream address is invalid".into()))
    }

    /// A new connection starts with nothing subscribed at Vortex; the
    /// manager subscribes everything again right after.
    fn on_connected(&mut self) -> Vec<Message> {
        self.subs.clear();
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        subs.iter().filter_map(|s| self.subscribe_one(s)).collect()
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        subs.iter()
            .filter_map(|s| {
                // Only what was sent is taken back, at the mode it has.
                let info = self.subs.remove(&Self::key(s)?)?;
                Some(Self::frame(&info.ticker, info.mode, "unsubscribe"))
            })
            .collect()
    }

    /// Vortex replaces a ticker's mode when it is subscribed again.
    fn mode_change_frames(
        &mut self,
        _old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        self.subscribe_one(new).into_iter().collect()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) if b.len() < 2 => vec![FeedEvent::Heartbeat],
            Message::Binary(b) => split_frame(b)
                .into_iter()
                .flat_map(|p| self.packet_events(p))
                .collect(),
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((PING_INTERVAL, Message::Ping(Vec::new())))
    }
}
