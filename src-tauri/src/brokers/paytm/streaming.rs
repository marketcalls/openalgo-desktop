//! Paytm Money market-data socket (web `streaming/paytm_websocket.py`,
//! `paytm_adapter.py`, `paytm_mapping.py`).
//!
//! * URL `wss://developer-ws.paytmmoney.com/broadcast/user/v1/data?x_jwt_token=<public_access_token>`
//!   (`paytm_websocket.py:232`); no header auth.
//! * Subscribe / unsubscribe is one JSON array of preferences
//!   `{"actionType":"ADD"|"REMOVE","modeType":"LTP"|"QUOTE"|"FULL",
//!   "scripType":"INDEX|EQUITY|ETF|FUTURE|OPTION","exchangeType":"NSE"|"BSE",
//!   "scripId":"<security_id>"}` (`:153-181`); mixed modes share a frame.
//! * Keep-alive is a protocol ping every 30 s (`HEART_BEAT_INTERVAL`,
//!   `run_forever(ping_interval=30, ping_timeout=10)`, `:255-259`).
//! * Binary frames are little-endian; byte 0 is the packet code
//!   (`:43-48`): 61 LTP (23 B), 62 QUOTE (67 B), 63 FULL (175 B),
//!   64 INDEX_LTP (23 B), 65 INDEX_QUOTE (43 B), 66 INDEX_FULL (39 B).
//!   Prices are `f32` rupees (no scaling). A frame may carry several packets
//!   back to back; each is cut by its code's size.
//! * Packets carry no exchange: the tick's symbol and exchange come from the
//!   subscription registered under the `security_id`.

use super::mapping::scrip_type;
use crate::brokers::common::streaming::{
    now_ms, round2, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const WS_URL: &str = "wss://developer-ws.paytmmoney.com/broadcast/user/v1/data";
/// Preferences per frame.
pub const BATCH: usize = 100;
/// web `HEART_BEAT_INTERVAL`.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

pub const CODE_LTP: u8 = 61;
pub const CODE_QUOTE: u8 = 62;
pub const CODE_FULL: u8 = 63;
pub const CODE_INDEX_LTP: u8 = 64;
pub const CODE_INDEX_QUOTE: u8 = 65;
pub const CODE_INDEX_FULL: u8 = 66;

/// Packet size for a packet code (`_parse_binary_data` docstring).
pub fn packet_len(code: u8) -> Option<usize> {
    Some(match code {
        CODE_LTP | CODE_INDEX_LTP => 23,
        CODE_QUOTE => 67,
        CODE_FULL => 175,
        CODE_INDEX_QUOTE => 43,
        CODE_INDEX_FULL => 39,
        _ => return None,
    })
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
    pref: Value,
}

pub struct PaytmFeed {
    url: String,
    subs: HashMap<u32, SubInfo>,
    symbols: SymbolResolver,
}

fn mode_type(mode: FeedMode) -> &'static str {
    match mode {
        FeedMode::Ltp => "LTP",
        FeedMode::Quote => "QUOTE",
        FeedMode::Depth => "FULL",
    }
}

fn f32le(b: &[u8], o: usize) -> f64 {
    round2(f64::from(f32::from_le_bytes([
        b[o],
        b[o + 1],
        b[o + 2],
        b[o + 3],
    ])))
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn i32le(b: &[u8], o: usize) -> i64 {
    i64::from(i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]))
}

fn i16le(b: &[u8], o: usize) -> i64 {
    i64::from(i16::from_le_bytes([b[o], b[o + 1]]))
}

/// A packet decoded to its fields (before symbol lookup).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Packet {
    pub code: u8,
    pub security_id: u32,
    pub tick: NormalizedTick,
    pub depth: Option<(Vec<DepthLevel>, Vec<DepthLevel>)>,
}

/// Decode one packet whose length matches its code.
pub fn decode_packet(p: &[u8]) -> Option<Packet> {
    let code = *p.first()?;
    if p.len() < packet_len(code)? {
        return None;
    }
    let mut t = NormalizedTick {
        ltp: f32le(p, 1),
        ..Default::default()
    };
    let mut depth = None;
    let id;
    match code {
        // LTP / INDEX_LTP (`:332-361`): ltp@1, ltt@5, id@9, tradable@13,
        // mode@14, change_abs@15, change_pct@19.
        CODE_LTP | CODE_INDEX_LTP => {
            t.last_trade_time_ms = i64::from(u32le(p, 5)) * 1000;
            id = u32le(p, 9);
            t.change = f32le(p, 15);
            t.change_percent = f32le(p, 19);
        }
        // QUOTE (`:363-393`): ltt@5, id@9, ltq@15, atp@19, volume@23,
        // tbq@27, tsq@31, open@35, close@39, high@43, low@47,
        // change_pct@51, change_abs@55.
        CODE_QUOTE => {
            t.last_trade_time_ms = i64::from(u32le(p, 5)) * 1000;
            id = u32le(p, 9);
            t.last_quantity = i64::from(u32le(p, 15));
            t.average_price = f32le(p, 19);
            t.volume = i64::from(u32le(p, 23));
            t.total_buy_quantity = i64::from(u32le(p, 27));
            t.total_sell_quantity = i64::from(u32le(p, 31));
            t.open = f32le(p, 35);
            t.close = f32le(p, 39);
            t.high = f32le(p, 43);
            t.low = f32le(p, 47);
            t.change_percent = f32le(p, 51);
            t.change = f32le(p, 55);
        }
        // INDEX_QUOTE (`:395-414`): id@5, open@11, close@15, high@19,
        // low@23, change_abs@27, change_pct@31.
        CODE_INDEX_QUOTE => {
            id = u32le(p, 5);
            t.open = f32le(p, 11);
            t.close = f32le(p, 15);
            t.high = f32le(p, 19);
            t.low = f32le(p, 23);
            t.change = f32le(p, 27);
            t.change_percent = f32le(p, 31);
        }
        // FULL (`:416-513`): 5 x 20-byte levels from @1 (buy_qty i32@0,
        // sell_qty i32@4, buy_orders i16@8, sell_orders i16@10,
        // buy_price f32@12, sell_price f32@16), then ltp@101, ltt@105,
        // id@109, ltq@115, atp@119, volume@123, tbq@127, tsq@131, open@135,
        // close@139, high@143, low@147, change_pct@151, change_abs@155,
        // oi u32@167, oi_change@171.
        CODE_FULL => {
            let mut buy = Vec::with_capacity(5);
            let mut sell = Vec::with_capacity(5);
            for i in 0..5 {
                let o = 1 + i * 20;
                buy.push(DepthLevel {
                    quantity: i32le(p, o),
                    price: f32le(p, o + 12),
                    orders: i16le(p, o + 8),
                });
                sell.push(DepthLevel {
                    quantity: i32le(p, o + 4),
                    price: f32le(p, o + 16),
                    orders: i16le(p, o + 10),
                });
            }
            depth = Some((buy, sell));
            t.ltp = f32le(p, 101);
            t.last_trade_time_ms = i64::from(u32le(p, 105)) * 1000;
            id = u32le(p, 109);
            t.last_quantity = i64::from(u32le(p, 115));
            t.average_price = f32le(p, 119);
            t.volume = i64::from(u32le(p, 123));
            t.total_buy_quantity = i64::from(u32le(p, 127));
            t.total_sell_quantity = i64::from(u32le(p, 131));
            t.open = f32le(p, 135);
            t.close = f32le(p, 139);
            t.high = f32le(p, 143);
            t.low = f32le(p, 147);
            t.change_percent = f32le(p, 151);
            t.change = f32le(p, 155);
            t.oi = i64::from(u32le(p, 167));
        }
        // INDEX_FULL (`:515-533`): id@5, open@11, close@15, high@19,
        // low@23, change_pct@27, change_abs@31, ltt@35.
        CODE_INDEX_FULL => {
            id = u32le(p, 5);
            t.open = f32le(p, 11);
            t.close = f32le(p, 15);
            t.high = f32le(p, 19);
            t.low = f32le(p, 23);
            t.change_percent = f32le(p, 27);
            t.change = f32le(p, 31);
            t.last_trade_time_ms = i64::from(u32le(p, 35)) * 1000;
        }
        _ => return None,
    }
    if t.change == 0.0 && t.change_percent == 0.0 {
        t.derive_change();
    }
    Some(Packet {
        code,
        security_id: id,
        tick: t,
        depth,
    })
}

/// Split a binary frame into packets by their codes' sizes.
pub fn decode_frame(data: &[u8]) -> Vec<Packet> {
    let mut out = Vec::new();
    let mut off = 0;
    while off < data.len() {
        let Some(len) = packet_len(data[off]) else {
            tracing::debug!("Paytm Money feed: unknown packet code {}", data[off]);
            break;
        };
        if off + len > data.len() {
            break;
        }
        if let Some(p) = decode_packet(&data[off..off + len]) {
            out.push(p);
        }
        off += len;
    }
    out
}

impl PaytmFeed {
    pub fn new(public_access_token: &str, symbols: SymbolResolver) -> Self {
        Self::with_url(WS_URL, public_access_token, symbols)
    }

    pub fn with_url(base: &str, public_access_token: &str, symbols: SymbolResolver) -> Self {
        Self {
            url: format!(
                "{}?x_jwt_token={}",
                base,
                urlencoding::encode(public_access_token)
            ),
            subs: HashMap::new(),
            symbols,
        }
    }

    /// The preference object for one subscription (web adapter
    /// `subscribe`).
    pub fn preference(&self, s: &FeedSubscription, action: &str) -> Value {
        let scrip = self
            .symbols
            .by_symbol(&s.exchange, &s.symbol)
            .map(|r| scrip_type(&r, true))
            .unwrap_or(match s.exchange.as_str() {
                "NSE_INDEX" | "BSE_INDEX" => "INDEX",
                "NFO" | "BFO" if s.symbol.ends_with("CE") || s.symbol.ends_with("PE") => "OPTION",
                "NFO" | "BFO" => "FUTURE",
                _ => "EQUITY",
            });
        let exchange_type = match s.brexchange.as_str() {
            "BSE" | "BFO" | "BSE_INDEX" => "BSE",
            _ => match s.exchange.as_str() {
                "BSE" | "BFO" | "BSE_INDEX" => "BSE",
                _ => "NSE",
            },
        };
        json!({
            "actionType": action,
            "modeType": mode_type(s.mode),
            "scripType": scrip,
            "exchangeType": exchange_type,
            "scripId": s.token,
        })
    }

    fn frames(prefs: Vec<Value>) -> Vec<Message> {
        prefs
            .chunks(BATCH)
            .map(|c| Message::Text(Value::Array(c.to_vec()).to_string()))
            .collect()
    }

    fn parse_binary(&self, data: &[u8]) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        let now = now_ms();
        for p in decode_frame(data) {
            let Some(sub) = self.subs.get(&p.security_id) else {
                continue;
            };
            let mut t = p.tick;
            t.symbol = sub.symbol.clone();
            t.exchange = sub.exchange.clone();
            t.mode = sub.mode.code();
            t.timestamp_ms = now;
            if let Some((buy, sell)) = p.depth {
                out.push(FeedEvent::Tick(t.clone()));
                out.push(FeedEvent::Depth(NormalizedDepth {
                    symbol: t.symbol,
                    exchange: t.exchange,
                    ltp: t.ltp,
                    buy,
                    sell,
                    total_buy_quantity: t.total_buy_quantity,
                    total_sell_quantity: t.total_sell_quantity,
                    timestamp_ms: now,
                }));
            } else {
                out.push(FeedEvent::Tick(t));
            }
        }
        out
    }
}

impl BrokerFeed for PaytmFeed {
    fn broker(&self) -> &'static str {
        "paytm"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Paytm Money feed address is invalid".into()))
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut prefs = Vec::new();
        for s in subs {
            let Ok(id) = s.token.trim().parse::<u32>() else {
                tracing::warn!("No Paytm Money security id for {}:{}", s.exchange, s.symbol);
                continue;
            };
            let pref = self.preference(s, "ADD");
            self.subs.insert(
                id,
                SubInfo {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    mode: s.mode,
                    pref: pref.clone(),
                },
            );
            prefs.push(pref);
        }
        Self::frames(prefs)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut prefs = Vec::new();
        for s in subs {
            let Ok(id) = s.token.trim().parse::<u32>() else {
                continue;
            };
            let mut pref = match self.subs.remove(&id) {
                Some(info) => info.pref,
                None => self.preference(s, "REMOVE"),
            };
            pref["actionType"] = json!("REMOVE");
            prefs.push(pref);
        }
        Self::frames(prefs)
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        // One frame: REMOVE the old mode and ADD the new one.
        let mut prefs = Vec::new();
        if let Ok(id) = old.token.trim().parse::<u32>() {
            let mut pref = match self.subs.remove(&id) {
                Some(info) => info.pref,
                None => self.preference(old, "REMOVE"),
            };
            pref["actionType"] = json!("REMOVE");
            prefs.push(pref);
        }
        let add = self.subscribe_frames(std::slice::from_ref(new));
        for m in add {
            if let Message::Text(t) = m {
                if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&t) {
                    prefs.extend(a);
                }
            }
        }
        Self::frames(prefs)
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            Message::Text(t) => {
                // Text frames are server notices (errors, acks); logged only.
                if let Ok(v) = serde_json::from_str::<Value>(t) {
                    if v.get("error").is_some() || v.get("message").is_some() {
                        tracing::warn!("Paytm Money feed notice: {}", v);
                    }
                }
                Vec::new()
            }
            Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((PING_INTERVAL, Message::Ping(Vec::new())))
    }
}
