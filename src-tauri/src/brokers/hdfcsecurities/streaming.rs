//! InvestRight market-data feed (web `streaming/hdfcsecurities_websocket.py`,
//! `hdfcsecurities_adapter.py`, `hdfcsecurities_mapping.py`).
//!
//! * URL `wss://developer.hdfcsec.com/wsapi/v1/session?token=<access>&api_key=<key>`
//!   plus `Authorization` and `User-Agent` headers, as the web sends them.
//! * Subscribe and unsubscribe are one JSON text frame each:
//!   `{"heart_beat": false, "subscribe": [{"scripId": "NSE_2885", "type": "ALL"}]}`
//!   and `{"heart_beat": false, "unSubscribe": [..]}` (capital S; the server
//!   ignores `unsubscribe`). At most 100 scrips per frame. Mode 1 is `LTP`,
//!   modes 2 and 3 are `ALL` (the feed has no middle tier).
//! * Heartbeat: `{"heart_beat": true}` every 10 seconds.
//! * Inbound frames are protobuf `GenericDTOList` (bare `GenericDTO` as a
//!   fallback). `packetType` selects the payload and the OpenAlgo exchange:
//!   `instrumentId` is unique only within an exchange, so ticks are keyed on
//!   `(exchange, token)`.
//! * `*_CIRC` and `*_OI` packets reuse `mbpData` for a partial refresh: every
//!   other field is a proto3 zero. They are merged into the last full packet
//!   for the instrument and never published on their own.

use super::mapping::ws_scrip_id;
use super::proto::{packet_type as pt, GenericDto, GenericDtoList};
use super::USER_AGENT;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use prost::Message as _;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// Scrips per subscribe frame (web `MAX_SCRIPS_PER_SUBSCRIBE`).
pub const BATCH: usize = 100;
/// web `HEARTBEAT_INTERVAL`.
pub const HEARTBEAT: Duration = Duration::from_secs(10);

/// What a packet carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Index,
    Mbp,
    /// Circuit-band refresh only.
    Circuit,
    /// Open-interest refresh only.
    Oi,
    Greek,
}

/// One decoded feed packet (web `_parse_packet` dict).
#[derive(Debug, Clone, PartialEq)]
pub struct Packet {
    pub token: i64,
    /// OpenAlgo exchange from `packetType`, when the type maps to one.
    pub exchange: Option<&'static str>,
    pub kind: Kind,
    pub ltp: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    /// Previous close.
    pub close: f64,
    pub volume: i64,
    pub ltq: i64,
    pub average_price: f64,
    pub total_buy_quantity: i64,
    pub total_sell_quantity: i64,
    pub oi: i64,
    pub lower_limit: f64,
    pub upper_limit: f64,
    /// Last trade time as sent (0 when absent).
    pub ltt: i64,
    /// Epoch ms (packet timestamp, or receive time).
    pub timestamp_ms: i64,
    /// Bid and ask levels, at most five each, when the packet had a book.
    pub depth: Option<(Vec<DepthLevel>, Vec<DepthLevel>)>,
}

impl Packet {
    fn empty(token: i64, exchange: Option<&'static str>, kind: Kind, ts: i64) -> Self {
        Self {
            token,
            exchange,
            kind,
            ltp: 0.0,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close: 0.0,
            volume: 0,
            ltq: 0,
            average_price: 0.0,
            total_buy_quantity: 0,
            total_sell_quantity: 0,
            oi: 0,
            lower_limit: 0.0,
            upper_limit: 0.0,
            ltt: 0,
            timestamp_ms: ts,
            depth: None,
        }
    }

    /// Fold a partial circuit / OI packet into this full one.
    pub fn merge_partial(&mut self, p: &Packet) {
        match p.kind {
            Kind::Circuit => {
                self.lower_limit = p.lower_limit;
                self.upper_limit = p.upper_limit;
            }
            Kind::Oi => self.oi = p.oi,
            _ => return,
        }
        self.timestamp_ms = p.timestamp_ms;
    }

    /// Accumulate a packet for a snapshot: a later zero never erases an
    /// earlier value (web `_collect_feed_snapshot`).
    pub fn accumulate(&mut self, p: &Packet) {
        fn keep_f(a: &mut f64, b: f64) {
            if b != 0.0 {
                *a = b;
            }
        }
        fn keep_i(a: &mut i64, b: i64) {
            if b != 0 {
                *a = b;
            }
        }
        keep_f(&mut self.ltp, p.ltp);
        keep_f(&mut self.open, p.open);
        keep_f(&mut self.high, p.high);
        keep_f(&mut self.low, p.low);
        keep_f(&mut self.close, p.close);
        keep_i(&mut self.volume, p.volume);
        keep_i(&mut self.ltq, p.ltq);
        keep_f(&mut self.average_price, p.average_price);
        keep_i(&mut self.total_buy_quantity, p.total_buy_quantity);
        keep_i(&mut self.total_sell_quantity, p.total_sell_quantity);
        keep_i(&mut self.oi, p.oi);
        keep_f(&mut self.lower_limit, p.lower_limit);
        keep_f(&mut self.upper_limit, p.upper_limit);
        keep_i(&mut self.ltt, p.ltt);
        if p.depth
            .as_ref()
            .is_some_and(|(b, s)| !b.is_empty() || !s.is_empty())
        {
            self.depth = p.depth.clone();
        }
        if p.kind == Kind::Mbp || p.kind == Kind::Index {
            self.kind = p.kind;
        }
        self.timestamp_ms = p.timestamp_ms;
    }
}

/// `packetType` -> OpenAlgo exchange (web `_PACKET_TYPE_EXCHANGE`).
pub fn packet_exchange(packet_type: i32) -> Option<&'static str> {
    Some(match packet_type {
        pt::NSE_CM_ALL | pt::NSE_CM_CIRC => "NSE",
        pt::NSE_CD_ALL | pt::NSE_CD_CIRC | pt::NSE_CD_OI => "CDS",
        pt::NSE_FO_ALL | pt::NSE_FO_CIRC | pt::NSE_FO_OI | pt::NSE_FO_GREEK => "NFO",
        pt::BSE_CM => "BSE",
        pt::BSE_FO_ALL | pt::BSE_FO_OI | pt::BSE_FO_GREEK => "BFO",
        pt::MCX_PKT => "MCX",
        pt::NSE_INDEX => "NSE_INDEX",
        pt::BSE_INDEX => "BSE_INDEX",
        _ => return None,
    })
}

/// Decode one `GenericDTO` (web `_parse_packet`). `None` for heartbeats,
/// packets without an instrument and types the feed does not price.
pub fn parse_packet(p: &GenericDto) -> Option<Packet> {
    let ty = p.packet_type;
    if ty == pt::HEARTBEAT || p.instrument_id == 0 {
        return None;
    }
    let ts = if p.packet_timestamp != 0 {
        p.packet_timestamp
    } else {
        now_ms()
    };
    let ex = packet_exchange(ty);
    match ty {
        pt::NSE_INDEX | pt::BSE_INDEX => {
            let d = p.index_data.clone().unwrap_or_default();
            let mut out = Packet::empty(
                p.instrument_id,
                ex,
                Kind::Index,
                if d.packet_time_stamp != 0 {
                    d.packet_time_stamp
                } else {
                    ts
                },
            );
            out.ltp = d.index_value;
            out.open = d.opening_index;
            out.high = d.high_index_value;
            out.low = d.low_index_value;
            out.close = d.closing_index;
            Some(out)
        }
        pt::NSE_FO_GREEK | pt::BSE_FO_GREEK => {
            Some(Packet::empty(p.instrument_id, ex, Kind::Greek, ts))
        }
        pt::NSE_CM_CIRC | pt::NSE_CD_CIRC | pt::NSE_FO_CIRC => {
            let m = p.mbp_data.clone().unwrap_or_default();
            let mut out = Packet::empty(p.instrument_id, ex, Kind::Circuit, ts);
            out.lower_limit = m.lower_circuit_limit;
            out.upper_limit = m.upper_circuit_limit;
            Some(out)
        }
        pt::NSE_CD_OI | pt::NSE_FO_OI | pt::BSE_FO_OI => {
            let m = p.mbp_data.clone().unwrap_or_default();
            let mut out = Packet::empty(p.instrument_id, ex, Kind::Oi, ts);
            out.oi = m.oi;
            Some(out)
        }
        _ => {
            let is_quote = matches!(
                ty,
                pt::NSE_CM_ALL
                    | pt::NSE_CD_ALL
                    | pt::NSE_FO_ALL
                    | pt::BSE_CM
                    | pt::BSE_FO_ALL
                    | pt::MCX_PKT
            );
            let m = match (&p.mbp_data, is_quote) {
                (Some(m), _) => m.clone(),
                (None, true) => Default::default(),
                (None, false) => return None,
            };
            let mut out = Packet::empty(p.instrument_id, ex, Kind::Mbp, ts);
            out.ltp = m.last_traded_price;
            out.open = m.open_price;
            out.high = m.high_price;
            out.low = m.low_price;
            out.close = m.closing_price;
            out.volume = m.volume_traded_today;
            out.ltq = m.last_trade_quantity;
            out.average_price = m.average_trade_price;
            out.total_buy_quantity = m.total_buy_quantity;
            out.total_sell_quantity = m.total_sell_quantity;
            out.oi = m.oi;
            out.lower_limit = m.lower_circuit_limit;
            out.upper_limit = m.upper_circuit_limit;
            out.ltt = m.last_trade_time;
            let (mut buy, mut sell) = (Vec::new(), Vec::new());
            for l in m
                .market_depth_dto_list
                .map(|d| d.market_depth_dto)
                .unwrap_or_default()
            {
                let level = DepthLevel {
                    price: l.price,
                    quantity: l.quantity,
                    orders: l.number_of_orders,
                };
                if l.buy_flag {
                    buy.push(level);
                } else {
                    sell.push(level);
                }
            }
            if !buy.is_empty() || !sell.is_empty() {
                buy.truncate(5);
                sell.truncate(5);
                out.depth = Some((buy, sell));
            }
            Some(out)
        }
    }
}

/// Decode a binary frame: `GenericDTOList`, or a bare `GenericDTO`.
pub fn decode_frame(data: &[u8]) -> Vec<Packet> {
    let packets = match GenericDtoList::decode(data) {
        Ok(list) if !list.generic_dto_list.is_empty() => list.generic_dto_list,
        _ => match GenericDto::decode(data) {
            Ok(one) => vec![one],
            Err(e) => {
                tracing::warn!(
                    "HDFC Securities feed frame could not be decoded ({} bytes): {}",
                    data.len(),
                    e
                );
                return Vec::new();
            }
        },
    };
    packets.iter().filter_map(parse_packet).collect()
}

/// Feed subscription type for an OpenAlgo mode.
pub fn sub_type(mode: FeedMode) -> &'static str {
    match mode {
        FeedMode::Ltp => "LTP",
        FeedMode::Quote | FeedMode::Depth => "ALL",
    }
}

/// `{"heart_beat": false, "subscribe": [...]}` frames, at most `BATCH`
/// scrips each, one type per frame.
pub fn subscribe_frames(items: &[(String, &'static str)], key: &str) -> Vec<Message> {
    let mut out = Vec::new();
    for ty in ["LTP", "ALL"] {
        let ids: Vec<&String> = items
            .iter()
            .filter(|(_, t)| *t == ty)
            .map(|(s, _)| s)
            .collect();
        for batch in ids.chunks(BATCH) {
            let list: Vec<_> = batch
                .iter()
                .map(|s| json!({"scripId": s, "type": ty}))
                .collect();
            out.push(Message::Text(
                json!({"heart_beat": false, key: list}).to_string(),
            ));
        }
    }
    out
}

/// Heartbeat frame.
pub fn heartbeat_frame() -> Message {
    Message::Text(json!({"heart_beat": true}).to_string())
}

/// Feed URL with query authentication.
pub fn feed_url(ws_base: &str, api_key: &str, token: &str) -> String {
    format!(
        "{}?token={}&api_key={}",
        ws_base,
        urlencoding::encode(token),
        urlencoding::encode(api_key)
    )
}

/// Handshake request: query auth plus the headers the web sends.
pub fn feed_request(url: &str, token: &str) -> Result<WsRequest> {
    let mut req = url
        .into_client_request()
        .map_err(|_| AppError::Internal("HDFC Securities feed address is invalid".into()))?;
    let h = req.headers_mut();
    if let Ok(v) = HeaderValue::from_str(token) {
        h.insert("Authorization", v);
    }
    h.insert("User-Agent", HeaderValue::from_static(USER_AGENT));
    Ok(req)
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct HdfcSecuritiesFeed {
    url: String,
    token: String,
    /// `(OpenAlgo exchange, token)` -> subscription.
    subs: HashMap<(String, i64), SubInfo>,
    /// Last full packet per subscribed instrument, for partial merges.
    /// Entries are removed with their subscription.
    last: HashMap<(String, i64), Packet>,
}

fn token_num(t: &str) -> Option<i64> {
    t.trim().parse().ok()
}

impl HdfcSecuritiesFeed {
    pub fn new(ws_base: &str, api_key: &str, token: &str, _symbols: SymbolResolver) -> Self {
        Self {
            url: feed_url(ws_base, api_key, token),
            token: token.to_string(),
            subs: HashMap::new(),
            last: HashMap::new(),
        }
    }

    /// Subscription key of a packet: its exchange when the type says it,
    /// else the only subscription with that token.
    fn key_of(&self, p: &Packet) -> Option<(String, i64)> {
        if let Some(ex) = p.exchange {
            let k = (ex.to_string(), p.token);
            return self.subs.contains_key(&k).then_some(k);
        }
        let mut it = self.subs.keys().filter(|(_, t)| *t == p.token);
        match (it.next(), it.next()) {
            (Some(k), None) => Some(k.clone()),
            _ => None,
        }
    }

    fn on_packet(&mut self, p: Packet, out: &mut Vec<FeedEvent>) {
        if p.kind == Kind::Greek {
            return;
        }
        let Some(key) = self.key_of(&p) else {
            return;
        };
        let p = match p.kind {
            Kind::Circuit | Kind::Oi => match self.last.get_mut(&key) {
                Some(snap) => {
                    snap.merge_partial(&p);
                    snap.clone()
                }
                // Nothing to attach a standalone band or OI value to.
                None => return,
            },
            _ => {
                self.last.insert(key.clone(), p.clone());
                p
            }
        };
        let Some(sub) = self.subs.get(&key) else {
            return;
        };
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: p.ltp,
            close: p.close,
            timestamp_ms: p.timestamp_ms,
            last_trade_time_ms: if p.ltt > 1_000_000_000_000 {
                p.ltt
            } else {
                p.ltt * 1000
            },
            ..Default::default()
        };
        if sub.mode >= FeedMode::Quote {
            t.open = p.open;
            t.high = p.high;
            t.low = p.low;
            t.volume = p.volume;
            t.last_quantity = p.ltq;
            t.average_price = p.average_price;
            t.total_buy_quantity = p.total_buy_quantity;
            t.total_sell_quantity = p.total_sell_quantity;
            t.oi = p.oi;
        }
        t.derive_change();
        let depth = (sub.mode == FeedMode::Depth)
            .then(|| p.depth.clone())
            .flatten()
            .map(|(buy, sell)| NormalizedDepth {
                symbol: t.symbol.clone(),
                exchange: t.exchange.clone(),
                ltp: t.ltp,
                buy: pad5(buy),
                sell: pad5(sell),
                total_buy_quantity: p.total_buy_quantity,
                total_sell_quantity: p.total_sell_quantity,
                timestamp_ms: p.timestamp_ms,
            });
        out.push(FeedEvent::Tick(t));
        if let Some(d) = depth {
            out.push(FeedEvent::Depth(d));
        }
    }

    fn items(&mut self, subs: &[FeedSubscription], register: bool) -> Vec<(String, &'static str)> {
        subs.iter()
            .filter_map(|s| {
                let Some(tok) = token_num(&s.token) else {
                    tracing::warn!(
                        "No HDFC Securities feed token for {}:{}",
                        s.exchange,
                        s.symbol
                    );
                    return None;
                };
                let key = (s.exchange.clone(), tok);
                if register {
                    self.subs.insert(
                        key,
                        SubInfo {
                            symbol: s.symbol.clone(),
                            exchange: s.exchange.clone(),
                            mode: s.mode,
                        },
                    );
                } else {
                    self.subs.remove(&key);
                    self.last.remove(&key);
                }
                Some((ws_scrip_id(&s.exchange, &s.token), sub_type(s.mode)))
            })
            .collect()
    }

    /// Subscriptions and cached snapshots held (tests check removal).
    pub fn tracked(&self) -> (usize, usize) {
        (self.subs.len(), self.last.len())
    }
}

/// Exactly five levels, zero padded.
pub fn pad5(mut v: Vec<DepthLevel>) -> Vec<DepthLevel> {
    v.truncate(5);
    v.resize(5, DepthLevel::default());
    v
}

impl BrokerFeed for HdfcSecuritiesFeed {
    fn broker(&self) -> &'static str {
        "hdfcsecurities"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        feed_request(&self.url, &self.token)
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let items = self.items(subs, true);
        subscribe_frames(&items, "subscribe")
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let items = self.items(subs, false);
        subscribe_frames(&items, "unSubscribe")
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => {
                let mut out = Vec::new();
                for p in decode_frame(b) {
                    self.on_packet(p, &mut out);
                }
                out
            }
            Message::Text(t) => {
                tracing::debug!("HDFC Securities feed text frame: {} bytes", t.len());
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, heartbeat_frame()))
    }
}

/// A log-safe description of a socket error. The error's `Display` can
/// echo the request (whose URL carries the session token), so only its
/// kind is ever logged.
pub fn ws_error_kind(e: &tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::Http(r) => format!("refused with HTTP {}", r.status().as_u16()),
        E::Io(io) => format!("network error ({:?})", io.kind()),
        E::Tls(_) => "secure connection failed".into(),
        E::ConnectionClosed | E::AlreadyClosed => "connection closed".into(),
        E::Protocol(_) => "protocol error".into(),
        E::Capacity(_) => "message too large".into(),
        E::Url(_) => "invalid address".into(),
        _ => "socket error".into(),
    }
}
