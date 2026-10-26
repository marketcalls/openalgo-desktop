//! Kite ticker (web `streaming/zerodha_websocket.py`, `zerodha_adapter.py`,
//! `zerodha_order_adapter.py`).
//!
//! * URL `wss://ws.kite.trade?api_key=..&access_token=..` (the access-token
//!   half of the stored `api_key:access_token`).
//! * Subscribe is two JSON frames: `{"a":"subscribe","v":[..]}` then
//!   `{"a":"mode","v":["full",[..]]}`, batches of 200 tokens.
//! * Binary frames are big-endian: `u16` packet count, then `u16` length +
//!   packet. Packet sizes: 8 (LTP), 28/32 (index quote/full), 44 (quote),
//!   184 (full with 5-level depth).
//! * The exchange of a tick comes from the subscription map, never from the
//!   token, so `NSE_INDEX:NIFTY` stays `NSE_INDEX`.
//! * Prices are paise (/100), CDS /10^7 and BCD /10^4 by segment.
//! * Text frames carry order postbacks (`{"type":"order"}`).

use super::mapping::{from_kite_quantity, instrument_token, map_status};
use crate::brokers::common::streaming::{
    now_ms, ping, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, WsRequest, WEB_PING_INTERVAL, WEB_PING_TIMEOUT,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const WS_URL: &str = "wss://ws.kite.trade";
/// Tokens per subscribe frame.
pub const BATCH: usize = 200;

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct KiteFeed {
    url: String,
    subs: HashMap<u32, SubInfo>,
    symbols: SymbolResolver,
}

impl KiteFeed {
    pub fn new(api_key: &str, access_token: &str, symbols: SymbolResolver) -> Self {
        Self::with_url(WS_URL, api_key, access_token, symbols)
    }

    pub fn with_url(
        base: &str,
        api_key: &str,
        access_token: &str,
        symbols: SymbolResolver,
    ) -> Self {
        Self {
            url: format!(
                "{}?api_key={}&access_token={}",
                base,
                urlencoding::encode(api_key),
                urlencoding::encode(access_token)
            ),
            subs: HashMap::new(),
            symbols,
        }
    }

    fn kite_mode(mode: FeedMode) -> &'static str {
        match mode {
            FeedMode::Ltp => "ltp",
            FeedMode::Quote => "quote",
            FeedMode::Depth => "full",
        }
    }

    fn tokens(&mut self, subs: &[FeedSubscription], register: bool) -> Vec<(u32, FeedMode)> {
        subs.iter()
            .filter_map(|s| {
                let t = instrument_token(&s.token);
                if t.is_none() {
                    tracing::warn!("No Kite instrument token for {}:{}", s.exchange, s.symbol);
                }
                let t = t?;
                if register {
                    self.subs.insert(
                        t,
                        SubInfo {
                            symbol: s.symbol.clone(),
                            exchange: s.exchange.clone(),
                            mode: s.mode,
                        },
                    );
                } else {
                    self.subs.remove(&t);
                }
                Some((t, s.mode))
            })
            .collect()
    }

    fn mode_frames(tokens: &[(u32, FeedMode)], with_subscribe: bool) -> Vec<Message> {
        let mut out = Vec::new();
        for mode in [FeedMode::Ltp, FeedMode::Quote, FeedMode::Depth] {
            let ts: Vec<u32> = tokens
                .iter()
                .filter(|(_, m)| *m == mode)
                .map(|(t, _)| *t)
                .collect();
            for batch in ts.chunks(BATCH) {
                if with_subscribe {
                    out.push(Message::Text(
                        json!({"a": "subscribe", "v": batch}).to_string(),
                    ));
                }
                out.push(Message::Text(
                    json!({"a": "mode", "v": [Self::kite_mode(mode), batch]}).to_string(),
                ));
            }
        }
        out
    }

    fn parse_binary(&self, data: &[u8]) -> Vec<FeedEvent> {
        if data.len() < 4 {
            // 1-byte frames are Kite's heartbeat.
            return vec![FeedEvent::Heartbeat];
        }
        let n = be_u16(data, 0) as usize;
        let mut off = 2;
        let mut out = Vec::new();
        for _ in 0..n {
            if off + 2 > data.len() {
                break;
            }
            let len = be_u16(data, off) as usize;
            off += 2;
            if off + len > data.len() {
                break;
            }
            self.parse_packet(&data[off..off + len], &mut out);
            off += len;
        }
        out
    }

    fn parse_packet(&self, p: &[u8], out: &mut Vec<FeedEvent>) {
        if p.len() < 8 {
            return;
        }
        let token = be_u32(p, 0);
        let Some(sub) = self.subs.get(&token) else {
            return;
        };
        let div = match token & 0xff {
            3 => 10_000_000.0, // CDS
            6 => 10_000.0,     // BCD
            _ => 100.0,
        };
        let price = |o: usize| f64::from(be_i32(p, o)) / div;
        let qty = |o: usize| i64::from(be_u32(p, o));
        let now = now_ms();
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: price(4),
            timestamp_ms: now,
            ..Default::default()
        };
        let mut depth = None;
        match p.len() {
            28 | 32 => {
                // Index packet: ltp, high, low, open, close, change[, ts].
                t.high = price(8);
                t.low = price(12);
                t.open = price(16);
                t.close = price(20);
                if p.len() == 32 {
                    t.last_trade_time_ms = i64::from(be_u32(p, 28)) * 1000;
                }
                t.derive_change();
            }
            n if n >= 44 => {
                t.last_quantity = qty(8);
                t.average_price = price(12);
                t.volume = qty(16);
                t.total_buy_quantity = qty(20);
                t.total_sell_quantity = qty(24);
                t.open = price(28);
                t.high = price(32);
                t.low = price(36);
                t.close = price(40);
                t.derive_change();
                if n >= 184 {
                    t.last_trade_time_ms = i64::from(be_u32(p, 44)) * 1000;
                    t.oi = qty(48);
                    let level = |o: usize| DepthLevel {
                        quantity: i64::from(be_u32(p, o)),
                        price: f64::from(be_i32(p, o + 4)) / div,
                        orders: i64::from(be_u16(p, o + 8)),
                    };
                    let buy: Vec<DepthLevel> = (0..5).map(|i| level(64 + i * 12)).collect();
                    let sell: Vec<DepthLevel> = (0..5).map(|i| level(124 + i * 12)).collect();
                    depth = Some(NormalizedDepth {
                        symbol: t.symbol.clone(),
                        exchange: t.exchange.clone(),
                        ltp: t.ltp,
                        total_buy_quantity: t.total_buy_quantity,
                        total_sell_quantity: t.total_sell_quantity,
                        buy,
                        sell,
                        timestamp_ms: now,
                    });
                }
            }
            _ => {}
        }
        out.push(FeedEvent::Tick(t));
        if let Some(d) = depth {
            out.push(FeedEvent::Depth(d));
        }
    }

    /// Order postback text frame -> normalised order update.
    pub fn parse_text(&self, text: &str) -> Vec<FeedEvent> {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return Vec::new();
        };
        match v.get("type").and_then(Value::as_str) {
            Some("order") => v
                .get("data")
                .map(|d| vec![FeedEvent::OrderUpdate(self.order_update(d))])
                .unwrap_or_default(),
            Some("error") => {
                let detail = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
                tracing::warn!("Kite ticker error: {}", detail);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn order_update(&self, d: &Value) -> OrderUpdate {
        let s = |k: &str| match d.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        let f = |k: &str| d.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let i = |k: &str| d.get(k).and_then(Value::as_i64).unwrap_or(0);
        let br = s("tradingsymbol");
        let ex = s("exchange");
        let lot = (ex == "MCX")
            .then(|| {
                self.symbols
                    .by_brsymbol(&ex, &br)
                    .map(|r| i64::from(r.lot_size))
            })
            .flatten();
        let units = |q: i64| from_kite_quantity(q, &br, &ex, lot);
        let status = s("status");
        OrderUpdate {
            orderid: s("order_id"),
            symbol: self.symbols.oa_symbol_or_raw(&br, &ex),
            exchange: ex.clone(),
            action: s("transaction_type"),
            quantity: units(i("quantity")),
            price: f("price"),
            trigger_price: f("trigger_price"),
            pricetype: s("order_type"),
            product: s("product"),
            order_status: map_status(&status),
            filled_quantity: units(i("filled_quantity")),
            pending_quantity: units(i("pending_quantity")),
            average_price: f("average_price"),
            rejection_reason: if status == "REJECTED" {
                s("status_message")
            } else {
                String::new()
            },
        }
    }
}

fn be_u16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

fn be_u32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn be_i32(b: &[u8], o: usize) -> i32 {
    i32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl BrokerFeed for KiteFeed {
    fn broker(&self) -> &'static str {
        "zerodha"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Kite ticker address is invalid".into()))
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let tokens = self.tokens(subs, true);
        Self::mode_frames(&tokens, true)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let tokens: Vec<u32> = self
            .tokens(subs, false)
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        tokens
            .chunks(BATCH)
            .map(|b| Message::Text(json!({"a": "unsubscribe", "v": b}).to_string()))
            .collect()
    }

    fn mode_change_frames(
        &mut self,
        _old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        // Kite switches a subscribed token's mode with one `mode` frame.
        let tokens = self.tokens(std::slice::from_ref(new), true);
        Self::mode_frames(&tokens, false)
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            Message::Text(t) => self.parse_text(t),
            _ => Vec::new(),
        }
    }

    /// The web pings the ticker every 30 s, answered within 10 s
    /// (`zerodha_websocket.py`: `PING_INTERVAL`, `PING_TIMEOUT`). Kite's own
    /// one-byte heartbeats keep a quiet socket alive too; the ping also
    /// finds a half-open one sooner than the stall watchdog.
    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((WEB_PING_INTERVAL, ping()))
    }

    fn heartbeat_reply_timeout(&self) -> Option<Duration> {
        Some(WEB_PING_TIMEOUT)
    }
}
