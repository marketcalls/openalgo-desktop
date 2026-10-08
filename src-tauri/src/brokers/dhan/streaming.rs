//! Dhan live market feed, 20-level depth feed and order-update feed (web
//! `streaming/dhan_websocket.py`, `dhan_adapter.py`, `dhan_mapping.py`,
//! `dhan_order_adapter.py`).
//!
//! Market feed (`wss://api-feed.dhan.co?version=2&token=..&clientId=..&authType=2`):
//! * Subscribe is JSON `{"RequestCode", "InstrumentCount", "InstrumentList":
//!   [{"ExchangeSegment","SecurityId"}]}`, at most 100 instruments a frame;
//!   codes 15/16 ticker, 17/18 quote, 21/22 full (5-level depth).
//! * Binary frames are little-endian and may carry several messages, each
//!   with an 8-byte header: `u8 code @0, u16 length @1 (header included),
//!   u8 segment @3, u32 security id @4`. Payload offsets below are from the
//!   end of the header:
//!   - 2 ticker: `f32 ltp @0, u32 ltt @4`
//!   - 4 quote: `f32 ltp @0, u16 ltq @4, u32 ltt @6, f32 atp @10,
//!     u32 volume @14, u32 total sell @18, u32 total buy @22, f32 open @26,
//!     f32 close @30, f32 high @34, f32 low @38`
//!   - 5 OI: `u32 oi @0`; 6 previous close: `f32 prev close @0, u32 prev oi @4`
//!     (kept and folded into later ticks, not published on their own)
//!   - 8 full: quote fields with `u32 oi @26, oi high @30, oi low @34`, then
//!     `f32 open @38, close @42, high @46, low @50`, then five 20-byte levels
//!     from @54: `u32 bid qty, u32 ask qty, u16 bid orders, u16 ask orders,
//!     f32 bid price, f32 ask price`
//!   - 50 disconnect: `u16 reason @0` (805: too many connections); 0 is a
//!     heartbeat.
//!
//! 20-level depth (`wss://depth-api-feed.dhan.co/twentydepth?...`, NSE and
//! NFO only): 12-byte header `u16 length @0, u8 code @2, u8 segment @3,
//! u32 security id @4, u32 sequence @8`; code 41 bids, 51 asks, each 20 rows
//! of `f64 price, u32 quantity, u32 orders`. Both sides are collected per
//! instrument and published together.
//!
//! Order updates (`wss://api-order-update.dhan.co`): a `LoginReq` JSON frame
//! after connect, then JSON `{"Type":"order_alert","Data":{...}}` frames.

use super::mapping::data_segment;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const WS_URL: &str = "wss://api-feed.dhan.co";
pub const DEPTH20_URL: &str = "wss://depth-api-feed.dhan.co/twentydepth";
pub const ORDER_UPDATE_URL: &str = "wss://api-order-update.dhan.co";
/// Instruments per subscribe frame on the market feed.
pub const BATCH: usize = 100;
/// Instruments per subscribe frame on the 20-depth feed.
pub const DEPTH20_BATCH: usize = 50;

pub const SUBSCRIBE_TICKER: u8 = 15;
pub const UNSUBSCRIBE_TICKER: u8 = 16;
pub const SUBSCRIBE_QUOTE: u8 = 17;
pub const UNSUBSCRIBE_QUOTE: u8 = 18;
pub const SUBSCRIBE_FULL: u8 = 21;
pub const UNSUBSCRIBE_FULL: u8 = 22;
pub const SUBSCRIBE_20_DEPTH: u8 = 23;
pub const UNSUBSCRIBE_20_DEPTH: u8 = 24;

/// OpenAlgo exchange -> Dhan numeric segment in binary headers (web
/// `SEGMENT_TO_EXCHANGE`; both index exchanges are 0, NCO's 6 is inferred).
pub fn segment_code(exchange: &str) -> Option<u8> {
    Some(match exchange {
        "NSE_INDEX" | "BSE_INDEX" => 0,
        "NSE" => 1,
        "NFO" => 2,
        "CDS" => 3,
        "BSE" => 4,
        "MCX" => 5,
        "NCO" => 6,
        "BCD" => 7,
        "BFO" => 8,
        _ => return None,
    })
}

fn le_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn le_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn le_f32(b: &[u8], o: usize) -> f64 {
    let v = f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    // The shortest decimal that round-trips the f32 (954.1, not
    // 954.0999755859375).
    format!("{}", v).parse().unwrap_or(f64::from(v))
}

fn le_f64(b: &[u8], o: usize) -> f64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    f64::from_le_bytes(a)
}

fn url_with(base: &str, token: &str, client_id: &str, version: bool) -> String {
    let enc = |s: &str| urlencoding::encode(s).into_owned();
    if version {
        format!(
            "{}?version=2&token={}&clientId={}&authType=2",
            base,
            enc(token),
            enc(client_id)
        )
    } else {
        format!(
            "{}?token={}&clientId={}&authType=2",
            base,
            enc(token),
            enc(client_id)
        )
    }
}

fn request(url: &str) -> Result<WsRequest> {
    url.into_client_request()
        .map_err(|_| AppError::Internal("Dhan feed address is invalid".into()))
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    oi: i64,
    prev_close: f64,
}

fn instrument_frames(code: u8, list: &[(&str, &str)], batch: usize) -> Vec<Message> {
    list.chunks(batch)
        .map(|chunk| {
            let instruments: Vec<Value> = chunk
                .iter()
                .map(|(seg, id)| json!({"ExchangeSegment": seg, "SecurityId": id}))
                .collect();
            Message::Text(
                json!({
                    "RequestCode": code,
                    "InstrumentCount": instruments.len(),
                    "InstrumentList": instruments,
                })
                .to_string(),
            )
        })
        .collect()
}

/// The 5-level market feed.
pub struct DhanFeed {
    url: String,
    subs: HashMap<(u8, u32), SubInfo>,
}

impl DhanFeed {
    pub fn new(access_token: &str, client_id: &str, _symbols: SymbolResolver) -> Self {
        Self::with_url(WS_URL, access_token, client_id)
    }

    pub fn with_url(base: &str, access_token: &str, client_id: &str) -> Self {
        Self {
            url: url_with(base, access_token, client_id, true),
            subs: HashMap::new(),
        }
    }

    fn codes(mode: FeedMode) -> (u8, u8) {
        match mode {
            FeedMode::Ltp => (SUBSCRIBE_TICKER, UNSUBSCRIBE_TICKER),
            FeedMode::Quote => (SUBSCRIBE_QUOTE, UNSUBSCRIBE_QUOTE),
            FeedMode::Depth => (SUBSCRIBE_FULL, UNSUBSCRIBE_FULL),
        }
    }

    fn frames(&mut self, subs: &[FeedSubscription], subscribe: bool) -> Vec<Message> {
        let mut by_mode: Vec<(FeedMode, Vec<(&'static str, String)>)> = Vec::new();
        for s in subs {
            let (Some(seg), Some(code), Ok(id)) = (
                data_segment(&s.exchange),
                segment_code(&s.exchange),
                s.token.trim().parse::<u32>(),
            ) else {
                tracing::warn!("No Dhan security id for {}:{}", s.exchange, s.symbol);
                continue;
            };
            if subscribe {
                self.subs.insert(
                    (code, id),
                    SubInfo {
                        symbol: s.symbol.clone(),
                        exchange: s.exchange.clone(),
                        oi: 0,
                        prev_close: 0.0,
                    },
                );
            } else {
                self.subs.remove(&(code, id));
            }
            match by_mode.iter_mut().find(|(m, _)| *m == s.mode) {
                Some((_, v)) => v.push((seg, id.to_string())),
                None => by_mode.push((s.mode, vec![(seg, id.to_string())])),
            }
        }
        let mut out = Vec::new();
        for (mode, list) in by_mode {
            let (sub, unsub) = Self::codes(mode);
            let refs: Vec<(&str, &str)> = list.iter().map(|(a, b)| (*a, b.as_str())).collect();
            out.extend(instrument_frames(
                if subscribe { sub } else { unsub },
                &refs,
                BATCH,
            ));
        }
        out
    }

    /// Subscription for an inbound packet: segment and token, then the token
    /// alone (web `_on_data_5depth`).
    fn find(&mut self, segment: u8, token: u32) -> Option<&mut SubInfo> {
        if self.subs.contains_key(&(segment, token)) {
            return self.subs.get_mut(&(segment, token));
        }
        let key = self.subs.keys().find(|(_, t)| *t == token).copied()?;
        self.subs.get_mut(&key)
    }

    /// Instruments registered (bounded by the subscriptions).
    pub fn subscription_count(&self) -> usize {
        self.subs.len()
    }

    /// Decode one binary frame.
    pub fn parse_binary(&mut self, data: &[u8]) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        let mut off = 0usize;
        while off + 8 <= data.len() {
            let code = data[off];
            let len = le_u16(data, off + 1) as usize;
            let segment = data[off + 3];
            let token = le_u32(data, off + 4);
            if len < 8 || off + len > data.len() {
                if len >= 8 {
                    tracing::debug!("Incomplete Dhan feed message");
                }
                break;
            }
            let p = &data[off + 8..off + len];
            off += len;
            match code {
                0 => out.push(FeedEvent::Heartbeat),
                2 | 4 | 8 => self.market_packet(code, segment, token, p, &mut out),
                5 if p.len() >= 4 => {
                    if let Some(s) = self.find(segment, token) {
                        s.oi = i64::from(le_u32(p, 0));
                    }
                }
                6 if p.len() >= 8 => {
                    if let Some(s) = self.find(segment, token) {
                        s.prev_close = le_f32(p, 0);
                    }
                }
                50 => {
                    let reason = if p.len() >= 2 { le_u16(p, 0) } else { 0 };
                    tracing::warn!(
                        "Dhan closed the market feed (code {}{})",
                        reason,
                        if reason == 805 {
                            ": maximum websocket connections exceeded"
                        } else {
                            ""
                        }
                    );
                }
                _ => {}
            }
        }
        out
    }

    fn market_packet(
        &mut self,
        code: u8,
        segment: u8,
        token: u32,
        p: &[u8],
        out: &mut Vec<FeedEvent>,
    ) {
        let min = match code {
            2 => 8,
            4 => 42,
            _ => 154,
        };
        if p.len() < min {
            tracing::debug!("Short Dhan feed packet (code {}, {} bytes)", code, p.len());
            return;
        }
        let Some(sub) = self.find(segment, token) else {
            return;
        };
        let now = now_ms();
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            ltp: le_f32(p, 0),
            timestamp_ms: now,
            ..Default::default()
        };
        match code {
            2 => {
                t.mode = FeedMode::Ltp.code();
                t.last_trade_time_ms = i64::from(le_u32(p, 4)) * 1000;
                t.oi = sub.oi;
                t.close = sub.prev_close;
            }
            _ => {
                t.mode = if code == 4 {
                    FeedMode::Quote.code()
                } else {
                    FeedMode::Depth.code()
                };
                t.last_quantity = i64::from(le_u16(p, 4));
                t.last_trade_time_ms = i64::from(le_u32(p, 6)) * 1000;
                t.average_price = le_f32(p, 10);
                t.volume = i64::from(le_u32(p, 14));
                t.total_sell_quantity = i64::from(le_u32(p, 18));
                t.total_buy_quantity = i64::from(le_u32(p, 22));
                let base = if code == 4 {
                    t.oi = sub.oi;
                    26
                } else {
                    t.oi = i64::from(le_u32(p, 26));
                    sub.oi = t.oi;
                    38
                };
                t.open = le_f32(p, base);
                t.close = le_f32(p, base + 4);
                t.high = le_f32(p, base + 8);
                t.low = le_f32(p, base + 12);
                if t.close == 0.0 {
                    t.close = sub.prev_close;
                }
            }
        }
        t.derive_change();
        if code == 8 {
            let mut buy = Vec::with_capacity(5);
            let mut sell = Vec::with_capacity(5);
            for i in 0..5 {
                let o = 54 + 20 * i;
                buy.push(DepthLevel {
                    price: le_f32(p, o + 12),
                    quantity: i64::from(le_u32(p, o)),
                    orders: i64::from(le_u16(p, o + 8)),
                });
                sell.push(DepthLevel {
                    price: le_f32(p, o + 16),
                    quantity: i64::from(le_u32(p, o + 4)),
                    orders: i64::from(le_u16(p, o + 10)),
                });
            }
            let depth = NormalizedDepth {
                symbol: t.symbol.clone(),
                exchange: t.exchange.clone(),
                ltp: t.ltp,
                buy,
                sell,
                total_buy_quantity: t.total_buy_quantity,
                total_sell_quantity: t.total_sell_quantity,
                timestamp_ms: now,
            };
            out.push(FeedEvent::Tick(t));
            out.push(FeedEvent::Depth(depth));
        } else {
            out.push(FeedEvent::Tick(t));
        }
    }
}

impl BrokerFeed for DhanFeed {
    fn broker(&self) -> &'static str {
        "dhan"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url)
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
            Message::Text(t) => {
                tracing::debug!(
                    "Dhan feed text frame: {}",
                    t.chars().take(120).collect::<String>()
                );
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}

// ---------------------------------------------------------------------------
// 20-level depth
// ---------------------------------------------------------------------------

type Side = Option<Vec<DepthLevel>>;

/// The 20-level depth feed (NSE and NFO).
pub struct Dhan20DepthFeed {
    url: String,
    subs: HashMap<(u8, u32), (String, String)>,
    /// Bid and ask halves waiting for their pair; keys are subscribed
    /// instruments only, so this is bounded by the subscriptions.
    pending: HashMap<(u8, u32), (Side, Side)>,
}

impl Dhan20DepthFeed {
    pub fn new(access_token: &str, client_id: &str, _symbols: SymbolResolver) -> Self {
        Self::with_url(DEPTH20_URL, access_token, client_id)
    }

    pub fn with_url(base: &str, access_token: &str, client_id: &str) -> Self {
        Self {
            url: url_with(base, access_token, client_id, false),
            subs: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    fn frames(&mut self, subs: &[FeedSubscription], subscribe: bool) -> Vec<Message> {
        let mut list: Vec<(&'static str, String)> = Vec::new();
        for s in subs {
            // web: 20-level depth exists for NSE and NFO only.
            if !matches!(s.exchange.as_str(), "NSE" | "NFO") {
                continue;
            }
            let (Some(seg), Some(code), Ok(id)) = (
                data_segment(&s.exchange),
                segment_code(&s.exchange),
                s.token.trim().parse::<u32>(),
            ) else {
                continue;
            };
            if subscribe {
                self.subs
                    .insert((code, id), (s.symbol.clone(), s.exchange.clone()));
            } else {
                self.subs.remove(&(code, id));
                self.pending.remove(&(code, id));
            }
            list.push((seg, id.to_string()));
        }
        let refs: Vec<(&str, &str)> = list.iter().map(|(a, b)| (*a, b.as_str())).collect();
        instrument_frames(
            if subscribe {
                SUBSCRIBE_20_DEPTH
            } else {
                UNSUBSCRIBE_20_DEPTH
            },
            &refs,
            DEPTH20_BATCH,
        )
    }

    fn levels(p: &[u8]) -> Vec<DepthLevel> {
        (0..20)
            .map(|i| {
                let o = i * 16;
                DepthLevel {
                    price: le_f64(p, o),
                    quantity: i64::from(le_u32(p, o + 8)),
                    orders: i64::from(le_u32(p, o + 12)),
                }
            })
            .collect()
    }

    /// Instruments registered and half-built books held.
    pub fn sizes(&self) -> (usize, usize) {
        (self.subs.len(), self.pending.len())
    }

    pub fn parse_binary(&mut self, data: &[u8]) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        let mut off = 0usize;
        while off + 12 <= data.len() {
            let len = le_u16(data, off) as usize;
            let code = data[off + 2];
            let segment = data[off + 3];
            let token = le_u32(data, off + 4);
            if len < 12 || off + len > data.len() {
                break;
            }
            let p = &data[off + 12..off + len];
            off += len;
            match code {
                0 => out.push(FeedEvent::Heartbeat),
                41 | 51 if p.len() >= 320 => {
                    let key = (segment, token);
                    let Some((symbol, exchange)) = self.subs.get(&key).cloned() else {
                        continue;
                    };
                    let entry = self.pending.entry(key).or_insert((None, None));
                    if code == 41 {
                        entry.0 = Some(Self::levels(p));
                    } else {
                        entry.1 = Some(Self::levels(p));
                    }
                    if entry.0.is_some() && entry.1.is_some() {
                        if let Some((Some(buy), Some(sell))) = self.pending.remove(&key) {
                            out.push(FeedEvent::Depth(NormalizedDepth {
                                symbol,
                                exchange,
                                ltp: 0.0,
                                total_buy_quantity: buy.iter().map(|l| l.quantity).sum(),
                                total_sell_quantity: sell.iter().map(|l| l.quantity).sum(),
                                buy,
                                sell,
                                timestamp_ms: now_ms(),
                            }));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }
}

impl BrokerFeed for Dhan20DepthFeed {
    fn broker(&self) -> &'static str {
        "dhan"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url)
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // Half-built books from the previous connection are stale.
        self.pending.clear();
        Vec::new()
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

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[20]
    }
}

// ---------------------------------------------------------------------------
// Order updates
// ---------------------------------------------------------------------------

/// The live order-update feed.
pub struct DhanOrderFeed {
    url: String,
    access_token: String,
    client_id: String,
    symbols: SymbolResolver,
}

impl DhanOrderFeed {
    pub fn new(access_token: &str, client_id: &str, symbols: SymbolResolver) -> Self {
        Self::with_url(ORDER_UPDATE_URL, access_token, client_id, symbols)
    }

    pub fn with_url(
        url: &str,
        access_token: &str,
        client_id: &str,
        symbols: SymbolResolver,
    ) -> Self {
        Self {
            url: url.to_string(),
            access_token: access_token.to_string(),
            client_id: client_id.to_string(),
            symbols,
        }
    }

    /// The frame that authenticates the socket (web `on_open_extra`).
    pub fn login_frame(&self) -> Message {
        Message::Text(
            json!({
                "LoginReq": {
                    "MsgCode": 42,
                    "ClientId": self.client_id,
                    "Token": self.access_token,
                },
                "UserType": "SELF",
            })
            .to_string(),
        )
    }

    /// One order-alert frame -> order update (web `normalize`).
    pub fn parse_text(&self, text: &str) -> Option<OrderUpdate> {
        let v: Value = serde_json::from_str(text).ok()?;
        if v.get("Type").and_then(Value::as_str) != Some("order_alert") {
            return None;
        }
        let d = v.get("Data")?;
        // Live frames are camelCase; the docs say PascalCase.
        let field = |keys: &[&str]| -> Option<&Value> {
            keys.iter().find_map(|k| d.get(*k)).filter(|v| !v.is_null())
        };
        let s = |keys: &[&str]| -> String {
            match field(keys) {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            }
        };
        let n = |keys: &[&str]| super::data::num(field(keys));
        let raw_status = s(&["status", "Status"]).to_ascii_uppercase();
        let order_status = match raw_status.as_str() {
            "TRANSIT" | "PENDING" => "open".to_string(),
            "REJECTED" => "rejected".into(),
            "CANCELLED" => "cancelled".into(),
            "TRADED" => "complete".into(),
            "EXPIRED" => "expired".into(),
            other => other.to_ascii_lowercase(),
        };
        let quantity = n(&["quantity", "Quantity"]) as i64;
        let traded = n(&["tradedQty", "TradedQty"]) as i64;
        let exch = s(&["exchange", "Exchange"]);
        let seg = s(&["segment", "Segment"]);
        let exchange = match (exch.as_str(), seg.as_str()) {
            ("NSE", "E") => "NSE",
            ("NSE", "D") => "NFO",
            ("NSE", "C") => "CDS",
            ("BSE", "E") => "BSE",
            ("BSE", "D") => "BFO",
            ("BSE", "C") => "BCD",
            ("MCX", "M") => "MCX",
            _ => exch.as_str(),
        }
        .to_string();
        let token = s(&["securityId", "SecurityId"]);
        let br = s(&["symbol", "Symbol"]);
        let symbol = self
            .symbols
            .by_token(&exchange, token.trim())
            .map(|r| r.symbol)
            .unwrap_or_else(|| self.symbols.oa_symbol_or_raw(&br, &exchange));
        let txn = s(&["txnType", "TxnType"]);
        let ot = s(&["orderType", "OrderType"]);
        let product = s(&["product", "Product"]);
        Some(OrderUpdate {
            orderid: s(&["orderNo", "OrderNo"]),
            symbol,
            exchange,
            action: match txn.as_str() {
                "B" => "BUY".into(),
                "S" => "SELL".into(),
                other => other.to_string(),
            },
            quantity,
            price: n(&["price", "Price"]),
            trigger_price: n(&["triggerPrice", "TriggerPrice"]),
            pricetype: match ot.as_str() {
                "LMT" => "LIMIT".into(),
                "MKT" => "MARKET".into(),
                "SL" => "SL".into(),
                "SLM" => "SL-M".into(),
                other => other.to_string(),
            },
            product: match product.as_str() {
                "C" => "CNC".into(),
                "I" => "MIS".into(),
                "M" | "F" => "NRML".into(),
                other => other.to_string(),
            },
            order_status,
            filled_quantity: traded,
            pending_quantity: (quantity - traded).max(0),
            average_price: n(&["avgTradedPrice", "AvgTradedPrice"]),
            rejection_reason: if raw_status == "REJECTED" {
                s(&["reasonDescription", "ReasonDescription"])
            } else {
                String::new()
            },
        })
    }
}

impl BrokerFeed for DhanOrderFeed {
    fn broker(&self) -> &'static str {
        "dhan"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url)
    }

    fn on_connected(&mut self) -> Vec<Message> {
        vec![self.login_frame()]
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self
                .parse_text(t)
                .map(|u| vec![FeedEvent::OrderUpdate(u)])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}
