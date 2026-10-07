//! Arrow streams (web `streaming/arrow_websocket.py`, `arrow_adapter.py`,
//! `arrow_mapping.py`, `arrow_order_adapter.py`).
//!
//! Market data, `wss://ds.arrow.trade?appID=..&token=..`:
//! * Subscribe is one JSON text frame whose instrument-array key equals the
//!   mode: `{"code":"sub","mode":"ltpc","ltpc":[26009]}`; unsubscribe uses
//!   `"code":"unsub"` with the mode the token was subscribed in
//!   (`arrow_websocket.py:331-357`). At most 100 tokens per frame.
//! * OpenAlgo modes map 1 -> `ltpc`, 2 -> `quote`, 3 -> `full`
//!   (`arrow_mapping.py:64-68`; LTP uses `ltpc` to also get the close).
//! * The client must send the text `PONG` every 3 s; the server ignores
//!   protocol pings and reaps a silent client (`arrow_websocket.py:65-70`).
//! * One big-endian binary packet per message, mode by length: 13 ltp,
//!   17 ltpc, 93 quote, 249 full (241 on the legacy layout). Prices are
//!   paise. Offsets (`arrow_websocket.py:497-540`):
//!   - all: token u32 @0, ltp u32 @4; bytes 8..13 carry a change flag and
//!     net change that are ignored (and recomputed);
//!   - 17-byte ltpc only: close u32 @13 (in larger packets @13 is ltq);
//!   - quote (`>II5xIIQQIIIIQIIQQQ`): ltq u32 @13, avg u32 @17, tbq u64 @21,
//!     tsq u64 @29, open @37, high @41, close @45 (before low), low @49,
//!     volume u64 @53, ltt u32 @61, time u32 @65, oi u64 @69, oi day high
//!     u64 @77, oi day low u64 @85;
//!   - full: lower limit u32 @93, upper limit u32 @97, then ten 14-byte
//!     levels (qty u64, price u32, orders u16) at @109 for 249-byte packets
//!     (8 reserved bytes) or @101 for 241-byte packets; levels 0-4 bids,
//!     5-9 asks.
//! * Ticks are reported under the subscription's OpenAlgo symbol and
//!   exchange (so `NSE_INDEX` stays `NSE_INDEX`).
//!
//! Order updates, `wss://order-updates.arrow.trade?appID=..&token=..`: JSON
//! text frames with `id` and `updateType == "ORDER_UPDATE"`, numbers sent
//! as strings; the client sends `PONG` every 3 s.

use super::mapping::{map_status, price_type_from_arrow, product_from_arrow, side_from_arrow};
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Tokens per subscribe frame (`MAX_TOKENS_PER_SUBSCRIBE`).
pub const BATCH: usize = 100;
/// Client heartbeat cadence and text (`HEARTBEAT_INTERVAL`, `HEARTBEAT_TEXT`).
pub const HEARTBEAT: Duration = Duration::from_secs(3);
pub const HEARTBEAT_TEXT: &str = "PONG";

const PRICE_SCALE: f64 = 100.0;

/// OpenAlgo mode -> Arrow subscription mode.
pub fn arrow_mode(mode: FeedMode) -> &'static str {
    match mode {
        FeedMode::Ltp => "ltpc",
        FeedMode::Quote => "quote",
        FeedMode::Depth => "full",
    }
}

fn ws_url(base: &str, app_id: &str, jwt: &str) -> String {
    format!(
        "{}?appID={}&token={}",
        base,
        urlencoding::encode(app_id),
        urlencoding::encode(jwt)
    )
}

fn request(url: &str) -> Result<WsRequest> {
    url.into_client_request()
        .map_err(|_| AppError::Internal("Arrow stream address is invalid".into()))
}

fn be_u16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

fn be_u32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn be_u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_be_bytes(a)
}

fn price(b: &[u8], o: usize) -> f64 {
    f64::from(be_u32(b, o)) / PRICE_SCALE
}

fn qty(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// One decoded packet (prices in rupees).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArrowPacket {
    pub token: u32,
    pub ltp: f64,
    pub close: Option<f64>,
    pub ltq: i64,
    pub average_price: f64,
    pub total_buy_quantity: i64,
    pub total_sell_quantity: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub volume: i64,
    /// Seconds since epoch (0 when absent).
    pub ltt: i64,
    pub feed_time: i64,
    pub oi: i64,
    pub lower_limit: f64,
    pub upper_limit: f64,
    pub depth: Option<(Vec<DepthLevel>, Vec<DepthLevel>)>,
    /// Packet carried the quote block.
    pub has_quote: bool,
}

/// Decode one binary packet (`_parse_packet`). `None` for frames shorter
/// than the 13-byte LTP packet.
pub fn decode_packet(data: &[u8]) -> Option<ArrowPacket> {
    let n = data.len();
    if n < 13 {
        return None;
    }
    let mut p = ArrowPacket {
        token: be_u32(data, 0),
        ltp: price(data, 4),
        ..Default::default()
    };
    if n < 93 {
        if n == 17 {
            p.close = Some(price(data, 13));
        }
        return Some(p);
    }
    p.has_quote = true;
    p.ltq = i64::from(be_u32(data, 13));
    p.average_price = price(data, 17);
    p.total_buy_quantity = qty(be_u64(data, 21));
    p.total_sell_quantity = qty(be_u64(data, 29));
    p.open = price(data, 37);
    p.high = price(data, 41);
    p.close = Some(price(data, 45));
    p.low = price(data, 49);
    p.volume = qty(be_u64(data, 53));
    p.ltt = i64::from(be_u32(data, 61));
    p.feed_time = i64::from(be_u32(data, 65));
    p.oi = qty(be_u64(data, 69));
    if n >= 241 {
        p.lower_limit = price(data, 93);
        p.upper_limit = price(data, 97);
        let off = if n >= 249 { 109 } else { 101 };
        if n >= off + 140 {
            let level = |i: usize| {
                let o = off + i * 14;
                DepthLevel {
                    quantity: qty(be_u64(data, o)),
                    price: price(data, o + 8),
                    orders: i64::from(be_u16(data, o + 12)),
                }
            };
            p.depth = Some(((0..5).map(level).collect(), (5..10).map(level).collect()));
        }
    }
    Some(p)
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct ArrowFeed {
    url: String,
    subs: HashMap<u32, SubInfo>,
}

impl ArrowFeed {
    pub fn new(base: &str, app_id: &str, jwt: &str) -> Self {
        Self {
            url: ws_url(base, app_id, jwt),
            subs: HashMap::new(),
        }
    }

    fn frames(code: &str, by_mode: Vec<(FeedMode, Vec<u32>)>) -> Vec<Message> {
        let mut out = Vec::new();
        for (mode, tokens) in by_mode {
            let m = arrow_mode(mode);
            for batch in tokens.chunks(BATCH) {
                out.push(Message::Text(
                    json!({"code": code, "mode": m, m: batch}).to_string(),
                ));
            }
        }
        out
    }

    fn group(items: Vec<(u32, FeedMode)>) -> Vec<(FeedMode, Vec<u32>)> {
        [FeedMode::Ltp, FeedMode::Quote, FeedMode::Depth]
            .into_iter()
            .map(|m| {
                (
                    m,
                    items
                        .iter()
                        .filter(|(_, x)| *x == m)
                        .map(|(t, _)| *t)
                        .collect::<Vec<u32>>(),
                )
            })
            .filter(|(_, v)| !v.is_empty())
            .collect()
    }

    fn token(s: &FeedSubscription) -> Option<u32> {
        let t = s.token.trim().parse::<u32>().ok();
        if t.is_none() {
            tracing::warn!("No Arrow token for {}:{}", s.exchange, s.symbol);
        }
        t
    }

    fn packet_events(&self, data: &[u8]) -> Vec<FeedEvent> {
        let Some(p) = decode_packet(data) else {
            return Vec::new();
        };
        let Some(sub) = self.subs.get(&p.token) else {
            return Vec::new();
        };
        let now = now_ms();
        let ts = if p.feed_time > 0 {
            p.feed_time * 1000
        } else {
            now
        };
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: p.ltp,
            close: p.close.unwrap_or(0.0),
            last_trade_time_ms: p.ltt * 1000,
            timestamp_ms: ts,
            ..Default::default()
        };
        if p.has_quote {
            t.open = p.open;
            t.high = p.high;
            t.low = p.low;
            t.volume = p.volume;
            t.average_price = p.average_price;
            t.last_quantity = p.ltq;
            t.total_buy_quantity = p.total_buy_quantity;
            t.total_sell_quantity = p.total_sell_quantity;
            t.oi = p.oi;
        }
        t.derive_change();
        let mut out = Vec::with_capacity(2);
        let depth = p.depth.map(|(buy, sell)| NormalizedDepth {
            symbol: t.symbol.clone(),
            exchange: t.exchange.clone(),
            ltp: t.ltp,
            buy,
            sell,
            total_buy_quantity: t.total_buy_quantity,
            total_sell_quantity: t.total_sell_quantity,
            timestamp_ms: ts,
        });
        out.push(FeedEvent::Tick(t));
        if let Some(d) = depth {
            out.push(FeedEvent::Depth(d));
        }
        out
    }

    /// Text frames are acks or errors; an error naming the session ends
    /// the feed until the trader logs in again.
    fn text_events(text: &str) -> Vec<FeedEvent> {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return Vec::new();
        };
        let status = v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if status != "error" && status != "failure" {
            return Vec::new();
        }
        let lower = text.to_ascii_lowercase();
        let auth = [
            "401",
            "403",
            "unauthorized",
            "forbidden",
            "invalid token",
            "token expired",
            "session expired",
            "invalid appid",
        ]
        .iter()
        .any(|k| lower.contains(k));
        if auth {
            return vec![FeedEvent::AuthFailed(
                "Arrow ended the live data session. Log in to Arrow again to resume streaming."
                    .into(),
            )];
        }
        let detail = v.get("message").and_then(|m| m.as_str()).unwrap_or("");
        tracing::warn!("Arrow stream error: {}", detail);
        Vec::new()
    }
}

impl BrokerFeed for ArrowFeed {
    fn broker(&self) -> &'static str {
        "arrow"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url)
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut items = Vec::new();
        for s in subs {
            let Some(t) = Self::token(s) else { continue };
            self.subs.insert(
                t,
                SubInfo {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    mode: s.mode,
                },
            );
            items.push((t, s.mode));
        }
        Self::frames("sub", Self::group(items))
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut items = Vec::new();
        for s in subs {
            let Some(t) = Self::token(s) else { continue };
            // Mirror the mode the token was subscribed in.
            let mode = self.subs.remove(&t).map(|i| i.mode).unwrap_or(s.mode);
            items.push((t, mode));
        }
        Self::frames("unsub", Self::group(items))
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.packet_events(b),
            Message::Text(t) => Self::text_events(t),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Text(HEARTBEAT_TEXT.into())))
    }
}

/// The order-update stream (no subscriptions; Arrow pushes every order of
/// the session).
pub struct ArrowOrderFeed {
    url: String,
    symbols: SymbolResolver,
}

impl ArrowOrderFeed {
    pub fn new(base: &str, app_id: &str, jwt: &str, symbols: SymbolResolver) -> Self {
        Self {
            url: ws_url(base, app_id, jwt),
            symbols,
        }
    }

    /// One order-update frame -> normalised update
    /// (`ArrowOrderUpdateAdapter.normalize`). Non-JSON text, frames without
    /// an `id` and other update types are ignored.
    pub fn parse_text(&self, text: &str) -> Option<OrderUpdate> {
        let d: Value = serde_json::from_str(text).ok()?;
        let s = |k: &str| match d.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(o) => o.to_string(),
        };
        let id = s("id");
        if id.is_empty() {
            return None;
        }
        if !matches!(d.get("updateType"), None | Some(Value::Null))
            && s("updateType") != "ORDER_UPDATE"
        {
            return None;
        }
        let num = |k: &str| -> Option<f64> {
            match d.get(k) {
                Some(Value::Number(n)) => n.as_f64(),
                Some(Value::String(x)) => x.trim().parse().ok(),
                _ => None,
            }
        };
        let raw_status = s("orderStatus").to_ascii_uppercase();
        let status = if raw_status.is_empty() {
            "open".to_string()
        } else {
            map_status(&raw_status)
        };
        let quantity = num("quantity").unwrap_or(0.0) as i64;
        let filled = num("cumulativeFillQty").unwrap_or(0.0) as i64;
        let pending = num("leavesQuantity")
            .map(|v| v as i64)
            .unwrap_or_else(|| (quantity - filled).max(0));
        let exchange = s("exchange");
        Some(OrderUpdate {
            orderid: id,
            symbol: self.symbols.oa_symbol_or_raw(&s("symbol"), &exchange),
            exchange,
            action: side_from_arrow(&s("transactionType")),
            quantity,
            price: num("price").unwrap_or(0.0),
            trigger_price: num("orderTriggerPrice").unwrap_or(0.0),
            pricetype: price_type_from_arrow(&s("order")),
            // The web order stream maps `M` to MIS while every REST path maps
            // it to NRML; the REST mapping is used so updates and books agree.
            product: product_from_arrow(&s("product")),
            filled_quantity: filled,
            pending_quantity: pending,
            average_price: num("averagePrice").unwrap_or(0.0),
            rejection_reason: if status == "rejected" {
                s("rejectionReason")
            } else {
                String::new()
            },
            order_status: status,
        })
    }
}

impl BrokerFeed for ArrowOrderFeed {
    fn broker(&self) -> &'static str {
        "arrow"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url)
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
                .map(FeedEvent::OrderUpdate)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Text(HEARTBEAT_TEXT.into())))
    }
}
