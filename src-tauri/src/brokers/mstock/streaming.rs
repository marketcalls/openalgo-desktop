//! mStock market-data socket (web `api/mstockwebsocket.py`,
//! `streaming/mstock_adapter.py`, `streaming/mstock_mapping.py`).
//!
//! * URL `wss://ws.mstock.trade?API_KEY=<private key>&ACCESS_TOKEN=<jwt>`
//!   (`mstockwebsocket.py:73,123-125`). On open the client sends the text
//!   frame `LOGIN:<jwt>`; mStock documents no acknowledgement, so the
//!   session counts as ready once LOGIN is sent (`:605-625`).
//! * Subscribe `{"action":1,"params":{"mode":m,"tokenList":[{"exchangeType":
//!   et,"tokens":[..]}]}}`, unsubscribe `action: 0`, one frame per mode with
//!   the tokens grouped by exchange type (`:726-840`). Exchange types
//!   (`mstock_mapping.py:13-25`): NSE 1, NFO 2, BSE 3, BFO 4, CDS 13, MCX 5
//!   (MCX undocumented by mStock), NSE_INDEX 1, BSE_INDEX 3, default 1.
//! * Keepalive: WebSocket ping every 20 s (`:64-68`).
//! * Binary frames (`parse_binary_message`, `:300-360`): a bare packet of
//!   51 / 123 / 379 bytes, or a 4-byte header `<H count, <H size` followed
//!   by `count` packets of `size` bytes.
//! * Packet layout, little-endian (`parse_binary_packet`, `:154-298`):
//!   `[0]` mode u8, `[1]` exchange type u8, `[2..27]` token ASCII
//!   (NUL-padded), `[27..35]` sequence u64, `[35..43]` exchange timestamp
//!   u64, `[43..51]` LTP u64 paise. Quote (123) adds `[51..59]` last traded
//!   qty u64, `[59..67]` average price u64 paise, `[67..75]` volume u64,
//!   `[75..83]` total buy qty f64, `[83..91]` total sell qty f64,
//!   `[91..99]` open, `[99..107]` high, `[107..115]` low, `[115..123]`
//!   close (u64 paise). Snap (379) adds `[123..131]` last trade time u64,
//!   `[131..139]` OI u64, `[139..147]` OI change % u64/100, `[147..347]`
//!   ten 20-byte depth levels (bids first five, asks last five; each
//!   `+2..+10` qty u64, `+10..+18` price u64 paise, `+18..+20` orders u16),
//!   `[347..355]` upper circuit, `[355..363]` lower circuit,
//!   `[363..371]` 52-week high, `[371..379]` 52-week low.
//!
//! mStock has no order-update stream.

use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedSubscription, Message, NormalizedDepth, NormalizedTick,
    WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::Secret;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const LTP_LEN: usize = 51;
pub const QUOTE_LEN: usize = 123;
pub const SNAP_LEN: usize = 379;
/// web `PING_INTERVAL`.
pub const PING_INTERVAL: Duration = Duration::from_secs(20);

/// web `MstockExchangeMapper.EXCHANGE_TYPES` (unknown -> NSE cash, 1).
pub fn exchange_type(exchange: &str) -> u8 {
    match exchange {
        "NSE" | "NSE_INDEX" => 1,
        "NFO" => 2,
        "BSE" | "BSE_INDEX" => 3,
        "BFO" => 4,
        "MCX" => 5,
        "CDS" => 13,
        _ => 1,
    }
}

/// One decoded packet. Prices are rupees.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Packet {
    pub mode: u8,
    pub exchange_type: u8,
    pub token: String,
    pub sequence: u64,
    pub exchange_timestamp: u64,
    pub ltp: f64,
    pub last_traded_qty: u64,
    pub avg_price: f64,
    pub volume: u64,
    pub total_buy_qty: f64,
    pub total_sell_qty: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub last_traded_timestamp: u64,
    pub oi: u64,
    pub oi_percent: f64,
    pub upper_circuit: f64,
    pub lower_circuit: f64,
    pub week_52_high: f64,
    pub week_52_low: f64,
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
}

fn u64le(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

fn f64le(b: &[u8], o: usize) -> f64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    f64::from_le_bytes(a)
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn paise(b: &[u8], o: usize) -> f64 {
    u64le(b, o) as f64 / 100.0
}

/// `parse_binary_packet`: a 51 / 123 / 379-byte packet, or a header-prefixed
/// frame of at least 383 bytes (first snap packet).
pub fn parse_packet(data: &[u8]) -> Option<Packet> {
    let p = match data.len() {
        LTP_LEN | QUOTE_LEN | SNAP_LEN => data,
        n if n >= SNAP_LEN + 4 => &data[4..4 + SNAP_LEN],
        _ => return None,
    };
    let token_bytes: Vec<u8> = {
        let raw = &p[2..27];
        let start = raw.iter().position(|b| *b != 0).unwrap_or(raw.len());
        let end = raw.iter().rposition(|b| *b != 0).map_or(start, |e| e + 1);
        raw[start..end].to_vec()
    };
    let mut q = Packet {
        mode: p[0],
        exchange_type: p[1],
        token: String::from_utf8_lossy(&token_bytes).into_owned(),
        sequence: u64le(p, 27),
        exchange_timestamp: u64le(p, 35),
        ltp: paise(p, 43),
        ..Default::default()
    };
    if p.len() >= QUOTE_LEN {
        q.last_traded_qty = u64le(p, 51);
        q.avg_price = paise(p, 59);
        q.volume = u64le(p, 67);
        q.total_buy_qty = f64le(p, 75);
        q.total_sell_qty = f64le(p, 83);
        q.open = paise(p, 91);
        q.high = paise(p, 99);
        q.low = paise(p, 107);
        q.close = paise(p, 115);
    }
    if p.len() >= SNAP_LEN {
        q.last_traded_timestamp = u64le(p, 123);
        q.oi = u64le(p, 131);
        q.oi_percent = paise(p, 139);
        q.upper_circuit = paise(p, 347);
        q.lower_circuit = paise(p, 355);
        q.week_52_high = paise(p, 363);
        q.week_52_low = paise(p, 371);
        let level = |o: usize| DepthLevel {
            quantity: i64::try_from(u64le(p, o + 2)).unwrap_or(i64::MAX),
            price: paise(p, o + 10),
            orders: i64::from(u16le(p, o + 18)),
        };
        q.bids = (0..5).map(|i| level(147 + i * 20)).collect();
        q.asks = (0..5).map(|i| level(147 + 100 + i * 20)).collect();
    }
    Some(q)
}

/// `parse_binary_message`: bare packets by exact size, otherwise the 4-byte
/// header; an impossible header size means one packet filling the frame,
/// and the count is clamped to what fits.
pub fn parse_frame(data: &[u8]) -> Vec<Packet> {
    if matches!(data.len(), LTP_LEN | QUOTE_LEN | SNAP_LEN) {
        return parse_packet(data).into_iter().collect();
    }
    if data.len() < 4 {
        return Vec::new();
    }
    let mut count = usize::from(u16le(data, 0));
    let mut size = usize::from(u16le(data, 2));
    if size == 0 || size > data.len() - 4 {
        size = data.len() - 4;
        if size == 0 {
            return Vec::new();
        }
    }
    let available = (data.len() - 4) / size;
    if count < 1 || count > available {
        count = available;
    }
    (0..count)
        .filter_map(|i| {
            let start = 4 + i * size;
            parse_packet(&data[start..start + size])
        })
        .collect()
}

/// `{"action": 1|0, "params": {"mode", "tokenList"}}` per mode.
pub fn sub_frames(subs: &[(u8, u8, String)], subscribe: bool) -> Vec<Message> {
    let mut by_mode: BTreeMap<u8, BTreeMap<u8, Vec<String>>> = BTreeMap::new();
    for (mode, et, token) in subs {
        by_mode
            .entry(*mode)
            .or_default()
            .entry(*et)
            .or_default()
            .push(token.clone());
    }
    by_mode
        .into_iter()
        .map(|(mode, groups)| {
            let list: Vec<Value> = groups
                .into_iter()
                .map(|(et, tokens)| json!({"exchangeType": et, "tokens": tokens}))
                .collect();
            Message::Text(
                json!({
                    "action": if subscribe { 1 } else { 0 },
                    "params": {"mode": mode, "tokenList": list},
                })
                .to_string(),
            )
        })
        .collect()
}

/// `wss://ws.mstock.trade?API_KEY=..&ACCESS_TOKEN=..`.
/// A host-only base gets a `/` path, so the request line is `GET /?...`.
pub fn feed_url(base: &str, private_key: &str, jwt: &str) -> String {
    let host_only = base
        .split_once("://")
        .is_some_and(|(_, rest)| !rest.contains('/'));
    format!(
        "{}{}?API_KEY={}&ACCESS_TOKEN={}",
        base,
        if host_only { "/" } else { "" },
        urlencoding::encode(private_key),
        urlencoding::encode(jwt)
    )
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
}

pub struct MstockFeed {
    base_url: String,
    jwt: Secret,
    private_key: Secret,
    /// `(exchange type, token)` -> subscription; removed on unsubscribe.
    subs: HashMap<(u8, String), SubInfo>,
}

impl MstockFeed {
    pub fn new(base_url: &str, jwt: &str, private_key: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            jwt: Secret::new(jwt),
            private_key: Secret::new(private_key),
            subs: HashMap::new(),
        }
    }

    /// Instruments currently registered (tests and hygiene).
    pub fn registered(&self) -> usize {
        self.subs.len()
    }

    fn frames(&mut self, subs: &[FeedSubscription], subscribe: bool) -> Vec<Message> {
        let mut list = Vec::with_capacity(subs.len());
        for s in subs {
            // The OpenAlgo exchange picks the segment: the master keeps
            // NSE / BSE as brexchange for currency rows, which would send
            // CDS tokens to the cash segment.
            let et = exchange_type(&s.exchange);
            let key = (et, s.token.clone());
            if subscribe {
                self.subs.insert(
                    key,
                    SubInfo {
                        symbol: s.symbol.clone(),
                        exchange: s.exchange.clone(),
                    },
                );
            } else {
                self.subs.remove(&key);
            }
            list.push((s.mode.code(), et, s.token.clone()));
        }
        sub_frames(&list, subscribe)
    }

    /// Normalised events of one binary frame.
    pub fn parse_binary(&self, data: &[u8]) -> Vec<FeedEvent> {
        let now = now_ms();
        let mut out = Vec::new();
        for q in parse_frame(data) {
            let Some(sub) = self.subs.get(&(q.exchange_type, q.token.clone())) else {
                continue;
            };
            let mut t = NormalizedTick {
                symbol: sub.symbol.clone(),
                exchange: sub.exchange.clone(),
                mode: q.mode.clamp(1, 3),
                ltp: q.ltp,
                last_trade_time_ms: i64::try_from(q.exchange_timestamp).unwrap_or(0),
                timestamp_ms: now,
                ..Default::default()
            };
            if q.mode >= 2 {
                t.last_quantity = i64::try_from(q.last_traded_qty).unwrap_or(0);
                t.average_price = q.avg_price;
                t.volume = i64::try_from(q.volume).unwrap_or(0);
                t.total_buy_quantity = q.total_buy_qty as i64;
                t.total_sell_quantity = q.total_sell_qty as i64;
                t.open = q.open;
                t.high = q.high;
                t.low = q.low;
                t.close = q.close;
                t.oi = i64::try_from(q.oi).unwrap_or(0);
                t.derive_change();
            }
            let depth = (q.mode == 3 && !q.bids.is_empty()).then(|| NormalizedDepth {
                symbol: t.symbol.clone(),
                exchange: t.exchange.clone(),
                ltp: t.ltp,
                buy: q.bids.clone(),
                sell: q.asks.clone(),
                total_buy_quantity: t.total_buy_quantity,
                total_sell_quantity: t.total_sell_quantity,
                timestamp_ms: now,
            });
            out.push(FeedEvent::Tick(t));
            if let Some(d) = depth {
                out.push(FeedEvent::Depth(d));
            }
        }
        out
    }
}

impl BrokerFeed for MstockFeed {
    fn broker(&self) -> &'static str {
        "mstock"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        feed_url(&self.base_url, self.private_key.expose(), self.jwt.expose())
            .into_client_request()
            .map_err(|_| AppError::Internal("mStock feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        vec![Message::Text(format!("LOGIN:{}", self.jwt.expose()))]
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, true)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, false)
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((PING_INTERVAL, Message::Ping(Vec::new())))
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}
