//! HDFC Sky market-data feed (web `streaming/hdfcsky_websocket.py`,
//! `hdfcsky_adapter.py`, `hdfcsky_mapping.py`).
//!
//! * URL `wss://developer.hdfcsky.com/wsapi/v1/session?token=<access>&api_key=<key>`;
//!   the gateway authenticates on the query string only (header auth 401s).
//! * Subscribe and unsubscribe are JSON text frames:
//!   `{"heart_beat": false, "subscribe": [{"scripId": "NSE_2885", "type": "ALL"}]}`
//!   and `{"heart_beat": false, "unSubscribe": [...]}` (capital S), at most
//!   100 scrips per frame. scripId is `<PREFIX>_<token>` with the prefixes
//!   `NSE, BSE, NFO, BFO, NCD (CDS), MCX, NSE_INDEX, BSE_INDEX`.
//! * Modes: OpenAlgo 1 -> `LTP`, 2 and 3 -> `ALL` (the full MBP packet
//!   carries OHLC, volume, OI and the 5-level book).
//! * Heartbeat `{"heart_beat": true}` every 10 s.
//! * Inbound frames are protobuf `GenericDTOList` (fallback: a bare
//!   `GenericDTO`); `packetType` picks `indexData`, `mbpData` or
//!   `greekData`. Prices are rupees, no scaling. No order-update stream.

use super::mapping::ws_scrip_id;
use super::proto::{packet_type as pt, GenericDto, GenericDtoList};
use super::USER_AGENT;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use prost::Message as _;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// Scrips per subscribe frame.
pub const BATCH: usize = 100;
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Subscription type for an OpenAlgo mode.
pub fn sub_type(mode: FeedMode) -> &'static str {
    match mode {
        FeedMode::Ltp => "LTP",
        FeedMode::Quote | FeedMode::Depth => "ALL",
    }
}

/// `{"heart_beat": false, "subscribe": [...]}` frames, grouped by type and
/// batched.
pub fn subscribe_messages(scrips: &[(String, &'static str)]) -> Vec<String> {
    let mut out = Vec::new();
    for ty in ["LTP", "ALL"] {
        let ids: Vec<&String> = scrips
            .iter()
            .filter(|(_, t)| *t == ty)
            .map(|(s, _)| s)
            .collect();
        for batch in ids.chunks(BATCH) {
            let list: Vec<Value> = batch
                .iter()
                .map(|s| json!({"scripId": s, "type": ty}))
                .collect();
            out.push(json!({"heart_beat": false, "subscribe": list}).to_string());
        }
    }
    out
}

/// `{"heart_beat": false, "unSubscribe": [...]}` frames.
pub fn unsubscribe_messages(scrips: &[(String, &'static str)]) -> Vec<String> {
    scrips
        .chunks(BATCH)
        .map(|batch| {
            let list: Vec<Value> = batch
                .iter()
                .map(|(s, t)| json!({"scripId": s, "type": t}))
                .collect();
            json!({"heart_beat": false, "unSubscribe": list}).to_string()
        })
        .collect()
}

pub fn heartbeat_message() -> String {
    json!({"heart_beat": true}).to_string()
}

/// Feed URL with the query authentication.
pub fn feed_url(base: &str, api_key: &str, token: &str) -> String {
    format!(
        "{}?token={}&api_key={}",
        base,
        urlencoding::encode(token),
        urlencoding::encode(api_key)
    )
}

/// Handshake request: query auth plus the headers the web client sends.
pub fn feed_request(base: &str, api_key: &str, token: &str) -> Result<WsRequest> {
    let mut req = feed_url(base, api_key, token)
        .into_client_request()
        .map_err(|_| AppError::Internal("HDFC Sky feed address is invalid".into()))?;
    let h = req.headers_mut();
    h.insert("User-Agent", HeaderValue::from_static(USER_AGENT));
    if let Ok(v) = HeaderValue::from_str(token) {
        h.insert("Authorization", v);
    }
    Ok(req)
}

/// What a packet carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Mbp,
    Index,
    Greek,
}

/// One decoded packet (web `_parse_packet` dict).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawTick {
    pub token: i64,
    pub packet_type: i32,
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
    /// Last trade time as sent.
    pub ltt: i64,
    /// Packet time, epoch ms.
    pub timestamp: i64,
    pub buy: Vec<DepthLevel>,
    pub sell: Vec<DepthLevel>,
}

impl RawTick {
    pub fn has_depth(&self) -> bool {
        !self.buy.is_empty() || !self.sell.is_empty()
    }

    /// Merge a later packet for the same instrument: a field the later
    /// packet left empty never wipes one an earlier packet filled (depth and
    /// OI arrive on separate packets).
    pub fn merge(&mut self, o: &RawTick) {
        fn f(a: &mut f64, b: f64) {
            if b != 0.0 {
                *a = b;
            }
        }
        fn i(a: &mut i64, b: i64) {
            if b != 0 {
                *a = b;
            }
        }
        self.token = o.token;
        self.packet_type = o.packet_type;
        self.kind = o.kind;
        f(&mut self.ltp, o.ltp);
        f(&mut self.open, o.open);
        f(&mut self.high, o.high);
        f(&mut self.low, o.low);
        f(&mut self.close, o.close);
        i(&mut self.volume, o.volume);
        i(&mut self.ltq, o.ltq);
        f(&mut self.average_price, o.average_price);
        i(&mut self.total_buy_quantity, o.total_buy_quantity);
        i(&mut self.total_sell_quantity, o.total_sell_quantity);
        i(&mut self.oi, o.oi);
        f(&mut self.lower_limit, o.lower_limit);
        f(&mut self.upper_limit, o.upper_limit);
        i(&mut self.ltt, o.ltt);
        i(&mut self.timestamp, o.timestamp);
        if o.has_depth() {
            self.buy = o.buy.clone();
            self.sell = o.sell.clone();
        }
    }
}

/// OpenAlgo exchange of a packet type (used to tell apart two subscribed
/// instruments that share a token number across segments).
pub fn packet_exchange(packet_type: i32) -> Option<&'static str> {
    Some(match packet_type {
        pt::NSE_CM_ALL | pt::NSE_CM_CIRC => "NSE",
        pt::NSE_CD_ALL | pt::NSE_CD_CIRC | pt::NSE_CD_OI => "CDS",
        pt::NSE_INDEX => "NSE_INDEX",
        pt::NSE_FO_ALL | pt::NSE_FO_CIRC | pt::NSE_FO_OI | pt::NSE_FO_GREEK => "NFO",
        pt::BSE_CM => "BSE",
        pt::BSE_INDEX => "BSE_INDEX",
        pt::BSE_FO_ALL | pt::BSE_FO_OI | pt::BSE_FO_GREEK => "BFO",
        pt::MCX_PKT => "MCX",
        _ => return None,
    })
}

fn is_mbp_packet(t: i32) -> bool {
    matches!(
        t,
        pt::NSE_CM_ALL
            | pt::NSE_CD_ALL
            | pt::NSE_FO_ALL
            | pt::BSE_CM
            | pt::BSE_FO_ALL
            | pt::MCX_PKT
            | pt::NSE_CM_CIRC
            | pt::NSE_CD_CIRC
            | pt::NSE_FO_CIRC
            | pt::NSE_CD_OI
            | pt::NSE_FO_OI
            | pt::BSE_FO_OI
    )
}

fn parse_packet(p: &GenericDto) -> Option<RawTick> {
    if p.packet_type == pt::HEARTBEAT || p.instrument_id == 0 {
        return None;
    }
    let mut t = RawTick {
        token: p.instrument_id,
        packet_type: p.packet_type,
        timestamp: if p.packet_timestamp != 0 {
            p.packet_timestamp
        } else {
            now_ms()
        },
        ..Default::default()
    };
    if matches!(p.packet_type, pt::NSE_INDEX | pt::BSE_INDEX) {
        let x = p.index_data.clone().unwrap_or_default();
        t.kind = Kind::Index;
        t.ltp = x.index_value;
        t.open = x.opening_index;
        t.high = x.high_index_value;
        t.low = x.low_index_value;
        t.close = x.closing_index;
        if x.packet_time_stamp != 0 {
            t.timestamp = x.packet_time_stamp;
        }
        return Some(t);
    }
    if matches!(p.packet_type, pt::NSE_FO_GREEK | pt::BSE_FO_GREEK) {
        t.kind = Kind::Greek;
        return Some(t);
    }
    if is_mbp_packet(p.packet_type) || p.mbp_data.is_some() {
        let m = p.mbp_data.clone().unwrap_or_default();
        t.kind = Kind::Mbp;
        t.ltp = m.last_traded_price;
        t.open = m.open_price;
        t.high = m.high_price;
        t.low = m.low_price;
        t.close = m.closing_price;
        t.volume = m.volume_traded_today;
        t.ltq = m.last_trade_quantity;
        t.average_price = m.average_trade_price;
        t.total_buy_quantity = m.total_buy_quantity;
        t.total_sell_quantity = m.total_sell_quantity;
        t.oi = m.oi;
        t.lower_limit = m.lower_circuit_limit;
        t.upper_limit = m.upper_circuit_limit;
        t.ltt = m.last_trade_time;
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
                t.buy.push(level);
            } else {
                t.sell.push(level);
            }
        }
        t.buy.truncate(5);
        t.sell.truncate(5);
        return Some(t);
    }
    None
}

/// Decode one binary frame (`GenericDTOList`, else a bare `GenericDTO`).
pub fn decode_frame(bytes: &[u8]) -> Vec<RawTick> {
    let packets = match GenericDtoList::decode(bytes) {
        Ok(l) if !l.generic_dto_list.is_empty() => l.generic_dto_list,
        _ => match GenericDto::decode(bytes) {
            Ok(p) => vec![p],
            Err(e) => {
                tracing::debug!("Undecodable HDFC Sky frame ({} bytes): {}", bytes.len(), e);
                return Vec::new();
            }
        },
    };
    packets.iter().filter_map(parse_packet).collect()
}

const AUTH_FAILURE: &[&str] = &[
    "401",
    "403",
    "unauthorized",
    "unauthorised",
    "forbidden",
    "invalid token",
    "token expired",
    "session expired",
    "invalid api_key",
];

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct HdfcSkyFeed {
    base: String,
    api_key: String,
    token: String,
    /// `(oa_exchange, token)` -> subscription.
    subs: HashMap<(String, i64), SubInfo>,
}

impl HdfcSkyFeed {
    pub fn new(base: &str, api_key: &str, token: &str) -> Self {
        Self {
            base: base.to_string(),
            api_key: api_key.to_string(),
            token: token.to_string(),
            subs: HashMap::new(),
        }
    }

    fn scrips(&mut self, subs: &[FeedSubscription], register: bool) -> Vec<(String, &'static str)> {
        subs.iter()
            .filter_map(|s| {
                let Ok(tok) = s.token.trim().parse::<i64>() else {
                    tracing::warn!("No HDFC Sky token for {}:{}", s.exchange, s.symbol);
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
                }
                Some((ws_scrip_id(&s.exchange, &s.token), sub_type(s.mode)))
            })
            .collect()
    }

    fn lookup(&self, t: &RawTick) -> Option<&SubInfo> {
        if let Some(ex) = packet_exchange(t.packet_type) {
            if let Some(s) = self.subs.get(&(ex.to_string(), t.token)) {
                return Some(s);
            }
        }
        self.subs
            .iter()
            .find(|((_, tok), _)| *tok == t.token)
            .map(|(_, s)| s)
    }

    /// Normalised events for one decoded packet (web `_normalize`).
    pub fn events(&self, t: &RawTick) -> Vec<FeedEvent> {
        if t.kind == Kind::Greek {
            return Vec::new();
        }
        let Some(sub) = self.lookup(t) else {
            return Vec::new();
        };
        let now = now_ms();
        let mut tick = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: t.ltp,
            close: t.close,
            last_trade_time_ms: if t.ltt != 0 { t.ltt } else { t.timestamp },
            timestamp_ms: now,
            ..Default::default()
        };
        if sub.mode >= FeedMode::Quote {
            tick.open = t.open;
            tick.high = t.high;
            tick.low = t.low;
            tick.volume = t.volume;
            tick.last_quantity = t.ltq;
            tick.average_price = t.average_price;
            tick.total_buy_quantity = t.total_buy_quantity;
            tick.total_sell_quantity = t.total_sell_quantity;
            tick.oi = t.oi;
        }
        tick.derive_change();
        let mut out = Vec::with_capacity(2);
        let depth = (sub.mode == FeedMode::Depth && t.has_depth()).then(|| NormalizedDepth {
            symbol: tick.symbol.clone(),
            exchange: tick.exchange.clone(),
            ltp: t.ltp,
            buy: t.buy.clone(),
            sell: t.sell.clone(),
            total_buy_quantity: t.total_buy_quantity,
            total_sell_quantity: t.total_sell_quantity,
            timestamp_ms: now,
        });
        out.push(FeedEvent::Tick(tick));
        if let Some(d) = depth {
            out.push(FeedEvent::Depth(d));
        }
        out
    }

    fn parse_text(&self, text: &str) -> Vec<FeedEvent> {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return Vec::new();
        };
        let status = v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if status == "error" || status == "failure" {
            let lower = text.to_ascii_lowercase();
            tracing::warn!("HDFC Sky feed reported an error");
            if AUTH_FAILURE.iter().any(|k| lower.contains(k)) {
                return vec![FeedEvent::AuthFailed(
                    "HDFC Sky ended the live data session. Log in to HDFC Sky again.".into(),
                )];
            }
        }
        Vec::new()
    }
}

impl BrokerFeed for HdfcSkyFeed {
    fn broker(&self) -> &'static str {
        "hdfcsky"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        feed_request(&self.base, &self.api_key, &self.token)
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let scrips = self.scrips(subs, true);
        subscribe_messages(&scrips)
            .into_iter()
            .map(Message::Text)
            .collect()
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let scrips = self.scrips(subs, false);
        unsubscribe_messages(&scrips)
            .into_iter()
            .map(Message::Text)
            .collect()
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        if sub_type(old.mode) == sub_type(new.mode) {
            // Same feed type (Quote <-> Depth): only the published shape
            // changes.
            self.scrips(std::slice::from_ref(new), true);
            return Vec::new();
        }
        let mut v = self.unsubscribe_frames(std::slice::from_ref(old));
        v.extend(self.subscribe_frames(std::slice::from_ref(new)));
        v
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => decode_frame(b)
                .iter()
                .flat_map(|t| self.events(t))
                .collect(),
            Message::Text(t) => self.parse_text(t),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT_INTERVAL, Message::Text(heartbeat_message())))
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
