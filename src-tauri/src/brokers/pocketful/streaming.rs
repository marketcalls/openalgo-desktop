//! Pocketful market-data socket (web `api/pocketfulwebsocket.py`,
//! `api/packet_decoder.py`, `streaming/pocketful_adapter.py`).
//!
//! * URL `wss://trade.pocketful.in/ws/v1/feeds?login_id=<client_id>&access_token=<token>`.
//! * Subscribe `{"a":"subscribe","v":[[exchange_code, token]],"m":<type>}`,
//!   one instrument per frame like the web; unsubscribe with
//!   `"a":"unsubscribe"`. Types: `marketdata` (detailed, mode 1),
//!   `compact_marketdata` (mode 2), `full_snapquote` (mode 4). OpenAlgo
//!   LTP -> compact, QUOTE -> detailed, DEPTH -> snapquote
//!   (`pocketful_adapter.py` `pocketful_mode_map`).
//! * Heartbeat: text `{"a":"h"}` every 15 s.
//! * Binary frames are big-endian; byte 0 is the mode. Prices are integer
//!   paise (/100); quantities raw. Offsets below cite `packet_decoder.py`.
//! * Modes 50 and 51 are order and trade updates: UTF-8 JSON after a 5-byte
//!   prefix (`decodeOrderUpdate`, `packet_decoder.py:279-302`).
//! * A packet is matched to its subscription by token and exchange code
//!   (`_find_subscription`); the tick is reported under the subscription's
//!   OpenAlgo symbol and exchange.

use super::mapping::{self, exchange_code, map_status, oa_pricetype, oa_product, text, text_any};
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

/// web `heartbeat_thread`: `{"a":"h"}` every 15 s.
pub const HEARTBEAT_SECS: u64 = 15;

/// Pocketful packet modes (byte 0).
pub const MODE_DETAILED: u8 = 1;
pub const MODE_COMPACT: u8 = 2;
pub const MODE_SNAPQUOTE: u8 = 4;
pub const MODE_ORDER: u8 = 50;
pub const MODE_TRADE: u8 = 51;

/// Subscription type (`"m"`) for a Pocketful mode.
pub fn market_type(pocketful_mode: u8) -> &'static str {
    match pocketful_mode {
        MODE_DETAILED => "marketdata",
        MODE_COMPACT => "compact_marketdata",
        _ => "full_snapquote",
    }
}

/// OpenAlgo mode -> Pocketful mode (web `pocketful_mode_map`).
pub fn pocketful_mode(mode: FeedMode) -> u8 {
    match mode {
        FeedMode::Ltp => MODE_COMPACT,
        FeedMode::Quote => MODE_DETAILED,
        FeedMode::Depth => MODE_SNAPQUOTE,
    }
}

/// One subscribe or unsubscribe frame.
pub fn sub_frame(action: &str, code: u8, token: u32, pocketful_mode: u8) -> Message {
    Message::Text(
        json!({"a": action, "v": [[code, token]], "m": market_type(pocketful_mode)}).to_string(),
    )
}

pub fn heartbeat_frame() -> Message {
    Message::Text(json!({"a": "h"}).to_string())
}

// ---------------------------------------------------------------------------
// Packet decoding (big-endian)
// ---------------------------------------------------------------------------

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn i32_at(b: &[u8], o: usize) -> i32 {
    i32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_be_bytes(a)
}

fn paise(v: u32) -> f64 {
    f64::from(v) / 100.0
}

/// Detailed market data (mode 1, at least 102 bytes;
/// `decodeDetailedMarketData`, `packet_decoder.py:160-186`). Prices in
/// rupees.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Detailed {
    pub exchange_code: u8,
    pub token: u32,
    pub ltp: f64,
    pub ltt: u32,
    pub ltq: i64,
    pub volume: i64,
    pub bid: f64,
    pub bid_qty: i64,
    pub ask: f64,
    pub ask_qty: i64,
    pub total_buy_qty: i64,
    pub total_sell_qty: i64,
    pub average_price: f64,
    pub exchange_ts: u32,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub year_high: f64,
    pub year_low: f64,
    pub oi: i64,
}

pub const DETAILED_LEN: usize = 102;
pub const COMPACT_LEN: usize = 42;
pub const SNAPQUOTE_LEN: usize = 166;

pub fn decode_detailed(b: &[u8]) -> Option<Detailed> {
    if b.len() < DETAILED_LEN {
        return None;
    }
    Some(Detailed {
        exchange_code: b[1],              // >b @1
        token: u32_at(b, 2),              // >I @2
        ltp: paise(u32_at(b, 6)),         // last_traded_price @6
        ltt: u32_at(b, 10),               // last_traded_time @10
        ltq: i64::from(u32_at(b, 14)),    // last_traded_quantity @14
        volume: i64::from(u32_at(b, 18)), // trade_volume @18
        bid: paise(u32_at(b, 22)),        // best_bid_price @22
        bid_qty: i64::from(u32_at(b, 26)),
        ask: paise(u32_at(b, 30)), // best_ask_price @30
        ask_qty: i64::from(u32_at(b, 34)),
        total_buy_qty: u64_at(b, 38) as i64,  // >Q @38
        total_sell_qty: u64_at(b, 46) as i64, // >Q @46
        average_price: paise(u32_at(b, 54)),  // average_trade_price @54
        exchange_ts: u32_at(b, 58),
        open: paise(u32_at(b, 62)),
        high: paise(u32_at(b, 66)),
        low: paise(u32_at(b, 70)),
        close: paise(u32_at(b, 74)),
        year_high: paise(u32_at(b, 78)),
        year_low: paise(u32_at(b, 82)),
        // lowDPR @86, highDPR @90
        oi: i64::from(u32_at(b, 94)), // currentOpenInterest @94
    })
}

/// Compact market data (mode 2, at least 42 bytes;
/// `decodeCompactMarketData`, `packet_decoder.py:252-265`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Compact {
    pub exchange_code: u8,
    pub token: u32,
    pub ltp: f64,
    pub change: f64,
    pub ltt: u32,
    pub oi: i64,
    pub bid: f64,
    pub ask: f64,
}

pub fn decode_compact(b: &[u8]) -> Option<Compact> {
    if b.len() < COMPACT_LEN {
        return None;
    }
    Some(Compact {
        exchange_code: b[1],
        token: u32_at(b, 2),
        ltp: paise(u32_at(b, 6)),
        // The web unpacks `change` as unsigned (`>I`), which turns a fall
        // into a huge rise; read as signed.
        change: f64::from(i32_at(b, 10)) / 100.0,
        ltt: u32_at(b, 14),
        // lowDPR @18, highDPR @22
        oi: i64::from(u32_at(b, 26)),
        // initialOpenInterest @30
        bid: paise(u32_at(b, 34)),
        ask: paise(u32_at(b, 38)),
    })
}

/// Snapquote (mode 4, at least 166 bytes; `decodeSnapquoteData`,
/// `packet_decoder.py:83-137`). No LTP: the web uses the average trade
/// price as `ltp` (`pocketful_adapter.py`, `data.py`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapquote {
    pub exchange_code: u8,
    pub token: u32,
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
    pub average_price: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub total_buy_qty: i64,
    pub total_sell_qty: i64,
    pub volume: i64,
}

pub fn decode_snapquote(b: &[u8]) -> Option<Snapquote> {
    if b.len() < SNAPQUOTE_LEN {
        return None;
    }
    // buyers @6, bidPrices @26, bidQtys @46, sellers @66, askPrices @86,
    // askQtys @106: five u32 each.
    let side = |orders: usize, prices: usize, qtys: usize| -> Vec<DepthLevel> {
        (0..5)
            .map(|i| DepthLevel {
                price: paise(u32_at(b, prices + 4 * i)),
                quantity: i64::from(u32_at(b, qtys + 4 * i)),
                orders: i64::from(u32_at(b, orders + 4 * i)),
            })
            .collect()
    };
    Some(Snapquote {
        exchange_code: b[1],
        token: u32_at(b, 2),
        bids: side(6, 26, 46),
        asks: side(66, 86, 106),
        average_price: paise(u32_at(b, 126)),
        open: paise(u32_at(b, 130)),
        high: paise(u32_at(b, 134)),
        low: paise(u32_at(b, 138)),
        close: paise(u32_at(b, 142)),
        total_buy_qty: u64_at(b, 146) as i64,
        total_sell_qty: u64_at(b, 154) as i64,
        volume: i64::from(u32_at(b, 162)),
    })
}

/// Order or trade update JSON after the 5-byte prefix.
pub fn decode_update(b: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(b.get(5..)?).ok()?;
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(Value::is_object)
}

/// A decoded market packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    Detailed(Detailed),
    Compact(Compact),
    Snapquote(Snapquote),
    Order(Value),
    Trade(Value),
}

impl Packet {
    pub fn token(&self) -> Option<(u8, u32)> {
        match self {
            Packet::Detailed(d) => Some((d.exchange_code, d.token)),
            Packet::Compact(c) => Some((c.exchange_code, c.token)),
            Packet::Snapquote(s) => Some((s.exchange_code, s.token)),
            _ => None,
        }
    }
}

fn jnum(v: &Value, keys: &[&str]) -> f64 {
    mapping::num_any(v, keys).unwrap_or(0.0)
}

fn jint(v: &Value, keys: &[&str]) -> i64 {
    jnum(v, keys) as i64
}

/// JSON market frames (the web's decoders accept them too): the decoded
/// key names, with prices in paise.
fn decode_json(v: &Value) -> Option<Packet> {
    let v = if v.get("instrument_token").is_some() || v.get("instrumentToken").is_some() {
        v
    } else if v.get("d").map(Value::is_object) == Some(true) {
        &v["d"]
    } else if v.get("data").map(Value::is_object) == Some(true) {
        &v["data"]
    } else {
        return None;
    };
    let mode = jint(v, &["mode"]);
    let code = jint(v, &["exchange_code", "exchangeCode"]) as u8;
    let token = jint(v, &["instrument_token", "instrumentToken"]) as u32;
    let p = |keys: &[&str]| jnum(v, keys) / 100.0;
    match mode as u8 {
        MODE_COMPACT => Some(Packet::Compact(Compact {
            exchange_code: code,
            token,
            ltp: p(&["last_traded_price"]),
            change: p(&["change"]),
            ltt: jint(v, &["last_traded_time"]) as u32,
            oi: jint(v, &["currentOpenInterest"]),
            bid: p(&["bidPrice"]),
            ask: p(&["askPrice"]),
        })),
        MODE_DETAILED => Some(Packet::Detailed(Detailed {
            exchange_code: code,
            token,
            ltp: p(&["last_traded_price"]),
            ltt: jint(v, &["last_traded_time"]) as u32,
            ltq: jint(v, &["last_traded_quantity"]),
            volume: jint(v, &["trade_volume"]),
            bid: p(&["best_bid_price"]),
            bid_qty: jint(v, &["best_bid_quantity"]),
            ask: p(&["best_ask_price"]),
            ask_qty: jint(v, &["best_ask_quantity"]),
            total_buy_qty: jint(v, &["total_buy_quantity"]),
            total_sell_qty: jint(v, &["total_sell_quantity"]),
            average_price: p(&["average_trade_price"]),
            open: p(&["open_price"]),
            high: p(&["high_price"]),
            low: p(&["low_price"]),
            close: p(&["close_price"]),
            oi: jint(v, &["currentOpenInterest"]),
            ..Default::default()
        })),
        MODE_SNAPQUOTE => {
            let arr = |k: &str| -> Vec<f64> {
                v[k].as_array()
                    .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
                    .unwrap_or_default()
            };
            let side = |orders: &str, prices: &str, qtys: &str| -> Vec<DepthLevel> {
                let (o, pr, q) = (arr(orders), arr(prices), arr(qtys));
                (0..5)
                    .map(|i| DepthLevel {
                        price: pr.get(i).copied().unwrap_or(0.0) / 100.0,
                        quantity: q.get(i).copied().unwrap_or(0.0) as i64,
                        orders: o.get(i).copied().unwrap_or(0.0) as i64,
                    })
                    .collect()
            };
            Some(Packet::Snapquote(Snapquote {
                exchange_code: code,
                token,
                bids: side("buyers", "bidPrices", "bidQtys"),
                asks: side("sellers", "askPrices", "askQtys"),
                average_price: p(&["averageTradePrice"]),
                open: p(&["open"]),
                high: p(&["high"]),
                low: p(&["low"]),
                close: p(&["close"]),
                total_buy_qty: jint(v, &["totalBuyQty"]),
                total_sell_qty: jint(v, &["totalSellQty"]),
                volume: jint(v, &["volume"]),
            }))
        }
        _ => None,
    }
}

/// Decode one socket frame (web `on_message`: JSON with a `mode` first,
/// else binary by byte 0).
pub fn decode_frame(msg: &Message) -> Option<Packet> {
    match msg {
        Message::Binary(b) => decode_binary(b),
        Message::Text(t) => {
            let v: Value = serde_json::from_str(t).ok()?;
            decode_json(&v)
        }
        _ => None,
    }
}

pub fn decode_binary(b: &[u8]) -> Option<Packet> {
    match *b.first()? {
        MODE_DETAILED => decode_detailed(b).map(Packet::Detailed),
        MODE_COMPACT => decode_compact(b).map(Packet::Compact),
        MODE_SNAPQUOTE => decode_snapquote(b).map(Packet::Snapquote),
        MODE_ORDER => decode_update(b).map(Packet::Order),
        MODE_TRADE => decode_update(b).map(Packet::Trade),
        _ => None,
    }
}

/// Exchange last-trade time to epoch ms, when it looks like epoch seconds.
fn ltt_ms(ltt: u32) -> i64 {
    // Anything before 2001 is not an epoch-seconds value; report 0.
    if ltt > 1_000_000_000 {
        i64::from(ltt) * 1000
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Feed
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct PocketfulFeed {
    url: String,
    /// (exchange code, token) -> subscription.
    subs: HashMap<(u8, u32), SubInfo>,
    symbols: SymbolResolver,
}

impl PocketfulFeed {
    pub fn new(
        ws_base: &str,
        client_id: &str,
        access_token: &str,
        symbols: SymbolResolver,
    ) -> Self {
        Self {
            url: feed_url(ws_base, client_id, access_token),
            subs: HashMap::new(),
            symbols,
        }
    }

    fn key(s: &FeedSubscription) -> Option<(u8, u32)> {
        let token = s.token.trim().parse::<u32>().ok();
        if token.is_none() {
            tracing::warn!(
                "No Pocketful instrument token for {}:{}",
                s.exchange,
                s.symbol
            );
        }
        Some((exchange_code(s.br_exchange()), token?))
    }

    fn tick_from(&self, sub: &SubInfo, p: &Packet) -> Vec<FeedEvent> {
        let now = now_ms();
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            timestamp_ms: now,
            ..Default::default()
        };
        match p {
            Packet::Compact(c) => {
                t.ltp = c.ltp;
                t.oi = c.oi;
                t.last_trade_time_ms = ltt_ms(c.ltt);
                t.change = mapping_round2(c.change);
                let prev = c.ltp - c.change;
                if prev > 0.0 && c.ltp > 0.0 {
                    t.change_percent = mapping_round2(c.change / prev * 100.0);
                }
                vec![FeedEvent::Tick(t)]
            }
            Packet::Detailed(d) => {
                t.ltp = d.ltp;
                t.open = d.open;
                t.high = d.high;
                t.low = d.low;
                t.close = d.close;
                t.volume = d.volume;
                t.last_quantity = d.ltq;
                t.average_price = d.average_price;
                t.total_buy_quantity = d.total_buy_qty;
                t.total_sell_quantity = d.total_sell_qty;
                t.oi = d.oi;
                t.last_trade_time_ms = ltt_ms(d.ltt);
                t.derive_change();
                vec![FeedEvent::Tick(t)]
            }
            Packet::Snapquote(s) => {
                t.ltp = s.average_price;
                t.open = s.open;
                t.high = s.high;
                t.low = s.low;
                t.close = s.close;
                t.volume = s.volume;
                t.total_buy_quantity = s.total_buy_qty;
                t.total_sell_quantity = s.total_sell_qty;
                t.derive_change();
                let depth = NormalizedDepth {
                    symbol: t.symbol.clone(),
                    exchange: t.exchange.clone(),
                    ltp: t.ltp,
                    buy: s.bids.clone(),
                    sell: s.asks.clone(),
                    total_buy_quantity: s.total_buy_qty,
                    total_sell_quantity: s.total_sell_qty,
                    timestamp_ms: now,
                };
                vec![FeedEvent::Tick(t), FeedEvent::Depth(depth)]
            }
            _ => Vec::new(),
        }
    }

    /// Order (mode 50) or trade (mode 51) update -> normalised order update.
    /// Field names follow the order book (`oms_order_id`, `order_status`,
    /// `trading_symbol`, ...).
    pub fn order_update(&self, d: &Value, trade: bool) -> Option<OrderUpdate> {
        let orderid = text_any(d, &["oms_order_id", "order_id", "orderId", "id"]);
        if orderid.is_empty() {
            return None;
        }
        let exchange = text(d, "exchange");
        let br = text_any(d, &["trading_symbol", "tradingsymbol", "symbol"]);
        let raw_status = text_any(d, &["order_status", "status"]);
        let status = if raw_status.is_empty() && trade {
            // A trade update is a fill; it carries no order status.
            "complete".to_string()
        } else {
            map_status(&raw_status, &text(d, "mode"))
        };
        let quantity = jint(d, &["quantity", "order_quantity"]);
        let filled = jint(d, &["filled_quantity", "fill_quantity", "trade_quantity"]);
        Some(OrderUpdate {
            orderid,
            symbol: if br.is_empty() || exchange.is_empty() {
                br.clone()
            } else {
                self.symbols.oa_symbol_or_raw(&br, &exchange)
            },
            exchange,
            action: text_any(d, &["order_side", "transaction_type"]).to_ascii_uppercase(),
            quantity,
            price: jnum(d, &["price", "trade_price"]),
            trigger_price: jnum(d, &["trigger_price"]),
            pricetype: oa_pricetype(&text(d, "order_type")),
            product: oa_product(&text(d, "product")),
            filled_quantity: filled,
            pending_quantity: mapping::num_any(d, &["remaining_quantity", "pending_quantity"])
                .map(|v| v as i64)
                .unwrap_or_else(|| (quantity - filled).max(0)),
            average_price: jnum(d, &["average_price", "average_trade_price", "avg_price"]),
            rejection_reason: if status == "rejected" {
                text_any(d, &["rejection_reason", "reject_reason", "reason"])
            } else {
                String::new()
            },
            order_status: status,
        })
    }

    pub fn handle(&self, p: Packet) -> Vec<FeedEvent> {
        match &p {
            Packet::Order(v) => self
                .order_update(v, false)
                .map(|u| vec![FeedEvent::OrderUpdate(u)])
                .unwrap_or_default(),
            Packet::Trade(v) => self
                .order_update(v, true)
                .map(|u| vec![FeedEvent::OrderUpdate(u)])
                .unwrap_or_default(),
            _ => {
                let Some(key) = p.token() else {
                    return Vec::new();
                };
                match self.subs.get(&key) {
                    Some(sub) => self.tick_from(sub, &p),
                    None => Vec::new(),
                }
            }
        }
    }
}

fn mapping_round2(v: f64) -> f64 {
    crate::brokers::common::streaming::round2(v)
}

/// `.../ws/v1/feeds?login_id=..&access_token=..`.
pub fn feed_url(ws_base: &str, client_id: &str, access_token: &str) -> String {
    format!(
        "{}/ws/v1/feeds?login_id={}&access_token={}",
        ws_base.trim_end_matches('/'),
        urlencoding::encode(client_id),
        urlencoding::encode(access_token)
    )
}

impl BrokerFeed for PocketfulFeed {
    fn broker(&self) -> &'static str {
        "pocketful"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Pocketful feed address is invalid".into()))
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut out = Vec::new();
        for s in subs {
            let Some(key) = Self::key(s) else { continue };
            self.subs.insert(
                key,
                SubInfo {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    mode: s.mode,
                },
            );
            out.push(sub_frame("subscribe", key.0, key.1, pocketful_mode(s.mode)));
        }
        out
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut out = Vec::new();
        for s in subs {
            let Some(key) = Self::key(s) else { continue };
            self.subs.remove(&key);
            out.push(sub_frame(
                "unsubscribe",
                key.0,
                key.1,
                pocketful_mode(s.mode),
            ));
        }
        out
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match decode_frame(msg) {
            Some(p) => self.handle(p),
            None => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((Duration::from_secs(HEARTBEAT_SECS), heartbeat_frame()))
    }
}

trait BrExchange {
    fn br_exchange(&self) -> &str;
}

impl BrExchange for FeedSubscription {
    /// web adapter: exchange code from the master `brexchange` (NSE/BSE for
    /// indices), falling back to the OpenAlgo exchange.
    fn br_exchange(&self) -> &str {
        if self.brexchange.is_empty() {
            &self.exchange
        } else {
            &self.brexchange
        }
    }
}
