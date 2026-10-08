//! Motilal Oswal broadcast feed and order stream (web
//! `api/motilal_websocket.py`, `streaming/motilal_adapter.py`,
//! `streaming/motilal_order_adapter.py`).
//!
//! Market data (`wss://ws1feed.motilaloswal.com/jwebsocket/jwebsocket`) is
//! binary, little-endian:
//! * Login (`motilal_websocket.py:355-395`), struct
//!   `=cHB15sB30sBBBB10sBBBBB45s`, 114 bytes: `'Q'`, u16 111, len(client
//!   code), client code space-padded to 15, len again, client code padded to
//!   30, `1,1,1`, len("1.0.0"), `"1.0.0"` padded to 10, `0,0,0,0,1`, 45
//!   spaces. No token travels; the first binary reply marks the session
//!   authenticated (`:402-430`).
//! * Register / unregister (`:966-992`), struct `=cHcciB`, 10 bytes: `'D'`,
//!   u16 7, exchange char (N NSE and NSEFO, B BSE, M MCX, C NSECD, D NCDEX,
//!   G BSEFO), segment char (`C`ash / `D`erivatives), i32 scrip, u8 1 add /
//!   0 remove.
//! * Inbound frames are concatenated 30-byte packets (`:453-486`): `[0]`
//!   exchange char, `[1..5]` i32 scrip, `[5..9]` i32 time, `[9]` type,
//!   `[10..30]` body. Types: `A` LTP (f32 rate, i32 last qty, i32 volume,
//!   f32 average price, i32 OI, `:651-690`); `B`..`F` depth level 1-5 (f32
//!   bid, i32 bid qty, i16 bid orders, f32 ask, i32 ask qty, i16 ask
//!   orders, `:595-649`); `G` day OHLC (f32 open, high, low, previous close,
//!   `:717-742`); `H` index (f32 rate, `:774-805`); `m` open interest (i32
//!   OI, high, low, `:744-772`); `W` circuit limits (f32 upper, lower,
//!   `:692-715`); `1` heartbeat. Rates are rupees (no scaling).
//! * The packet header carries only the exchange character, so NSE cash
//!   and NSE F&O share `N`; packets resolve back to the full exchange
//!   through the registrations (first registration wins, `:942-966`).
//!
//! Order updates (`wss://openapi.motilaloswal.com/ws`) are JSON: after the
//! socket opens send `{"clientid","authtoken","apikey"}` then
//! `{"clientid","action":"OrderSubscribe"}`; heartbeat
//! `{"clientid","action":"heartbeat"}` every 30 s; order frames carry
//! `orderstatus` and `uniqueorderid` with rupee prices.

use super::mapping::{self, token_text, vf, vi, vs};
use super::MotilalSession;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::Secret;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const PACKET_LEN: usize = 30;
pub const LOGIN_LEN: usize = 114;
pub const REGISTER_LEN: usize = 10;
/// web `_HEARTBEAT_SECONDS` of the order adapter.
pub const ORDER_HEARTBEAT: Duration = Duration::from_secs(30);
const VERSION: &str = "1.0.0";

/// Motilal exchange name -> wire character (`_map_exchange_to_char`).
pub fn exchange_char(exchange: &str) -> u8 {
    match exchange.to_ascii_uppercase().as_str() {
        "NSE" | "NSEFO" => b'N',
        "BSE" => b'B',
        "MCX" => b'M',
        "NSECD" => b'C',
        "NCDEX" => b'D',
        "BSEFO" => b'G',
        other => other.bytes().next().unwrap_or(b'N'),
    }
}

/// Lossy reverse (`_map_exchange_back`), for unregistered packets.
pub fn exchange_from_char(c: u8) -> String {
    match c {
        b'N' => "NSE".into(),
        b'B' => "BSE".into(),
        b'M' => "MCX".into(),
        b'C' => "NSECD".into(),
        b'D' => "NCDEX".into(),
        b'G' => "BSEFO".into(),
        other => (other as char).to_string(),
    }
}

/// The Motilal exchange a subscription registers on: the index rows use
/// their real exchange, everything else the `map_exchange` name.
pub fn feed_exchange(oa_exchange: &str) -> &str {
    match oa_exchange {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        "MCX_INDEX" => "MCX",
        other => mapping::map_exchange(other),
    }
}

fn pad(s: &str, n: usize) -> Vec<u8> {
    let mut b: Vec<u8> = s.bytes().take(n).collect();
    b.resize(n, b' ');
    b
}

/// The 114-byte login packet.
pub fn login_packet(client_code: &str) -> Vec<u8> {
    let len = client_code.len().min(255) as u8;
    let mut p = Vec::with_capacity(LOGIN_LEN);
    p.push(b'Q');
    p.extend_from_slice(&111u16.to_le_bytes());
    p.push(len);
    p.extend(pad(client_code, 15));
    p.push(len);
    p.extend(pad(client_code, 30));
    p.extend_from_slice(&[1, 1, 1]);
    p.push(VERSION.len() as u8);
    p.extend(pad(VERSION, 10));
    p.extend_from_slice(&[0, 0, 0, 0, 1]);
    p.extend(pad("", 45));
    p
}

/// The 10-byte register (`add`) or unregister packet.
pub fn register_packet(exchange: &str, segment: &str, scrip: i32, add: bool) -> Vec<u8> {
    let mut p = Vec::with_capacity(REGISTER_LEN);
    p.push(b'D');
    p.extend_from_slice(&7u16.to_le_bytes());
    p.push(exchange_char(exchange));
    p.push(segment.bytes().next().unwrap_or(b'C').to_ascii_uppercase());
    p.extend_from_slice(&scrip.to_le_bytes());
    p.push(u8::from(add));
    p
}

/// Index register / unregister text frame (web `register_index`; the web
/// marks the frame itself unverified).
pub fn index_frame(client_code: &str, exchange: &str, add: bool) -> String {
    json!({
        "clientid": client_code,
        "action": if add { "IndexRegister" } else { "IndexUnregister" },
        "exchange": exchange,
    })
    .to_string()
}

/// One decoded 30-byte packet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Packet {
    pub exchange: u8,
    pub scrip: i32,
    pub time: i32,
    pub kind: u8,
    pub body: [u8; 20],
}

fn i32_at(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn f32_at(b: &[u8], o: usize) -> f64 {
    f64::from(f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]))
}

fn i16_at(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Split a frame into packets (trailing partial packets are ignored).
pub fn packets(frame: &[u8]) -> Vec<Packet> {
    frame
        .chunks_exact(PACKET_LEN)
        .map(|p| {
            let mut body = [0u8; 20];
            body.copy_from_slice(&p[10..30]);
            Packet {
                exchange: p[0],
                scrip: i32_at(p, 1),
                time: i32_at(p, 5),
                kind: p[9],
                body,
            }
        })
        .collect()
}

/// Everything known about one scrip (web `last_quotes`, `last_depth`,
/// `last_oi`, `last_index`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScripData {
    pub ltp: f64,
    pub ltq: i64,
    pub volume: i64,
    pub avg_price: f64,
    pub oi: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub prev_close: f64,
    pub upper_circuit: f64,
    pub lower_circuit: f64,
    pub index_rate: Option<f64>,
    pub has_ltp: bool,
    pub has_ohlc: bool,
    pub has_oi: bool,
    pub bids: [Option<DepthLevel>; 5],
    pub asks: [Option<DepthLevel>; 5],
}

impl ScripData {
    /// web `has_snapshot`.
    pub fn complete(&self, need_oi: bool, whole_book: bool) -> bool {
        if !self.has_ltp || !self.has_ohlc {
            return false;
        }
        if need_oi && !self.has_oi {
            return false;
        }
        let full = self.bids.iter().all(Option::is_some) && self.asks.iter().all(Option::is_some);
        if whole_book || full {
            return full;
        }
        match (&self.bids[0], &self.asks[0]) {
            (Some(b), Some(a)) => b.price != 0.0 && a.price != 0.0,
            _ => false,
        }
    }

    /// First level with a price (web `_best_bid_ask`).
    pub fn best(levels: &[Option<DepthLevel>; 5]) -> DepthLevel {
        levels
            .iter()
            .flatten()
            .find(|l| l.price != 0.0)
            .copied()
            .unwrap_or_default()
    }

    pub fn levels(levels: &[Option<DepthLevel>; 5]) -> Vec<DepthLevel> {
        levels.iter().map(|l| l.unwrap_or_default()).collect()
    }
}

/// Decoded feed state for registered scrips, keyed by the full Motilal
/// exchange and scrip code. Bounded by the registrations: unregistering a
/// scrip drops its data.
#[derive(Debug, Default)]
pub struct FeedState {
    /// `(wire char, scrip)` -> full Motilal exchange.
    registered: HashMap<(u8, i32), String>,
    data: HashMap<(String, i32), ScripData>,
    /// Keep packets of unregistered scrips (index broadcasts on a
    /// short-lived quote socket). The live feed leaves this off.
    pub accept_unregistered: bool,
    authenticated: bool,
}

impl FeedState {
    /// A state that keeps packets of unregistered scrips when asked
    /// (index broadcasts on a short-lived quote socket).
    pub fn new(accept_unregistered: bool) -> Self {
        Self {
            accept_unregistered,
            ..Default::default()
        }
    }

    /// Record a registration; returns false if the wire key already belongs
    /// to another exchange (NSE vs NSEFO, first wins as on the web).
    pub fn register(&mut self, exchange: &str, scrip: i32) -> bool {
        let ex = exchange.to_ascii_uppercase();
        let key = (exchange_char(&ex), scrip);
        match self.registered.get(&key) {
            Some(existing) if *existing != ex => {
                tracing::warn!(
                    "Motilal scrip {} is registered on {} and {}; packets are kept under {}",
                    scrip,
                    existing,
                    ex,
                    existing
                );
                false
            }
            _ => {
                self.registered.insert(key, ex);
                true
            }
        }
    }

    pub fn unregister(&mut self, exchange: &str, scrip: i32) {
        let ex = exchange.to_ascii_uppercase();
        let key = (exchange_char(&ex), scrip);
        if self.registered.get(&key) == Some(&ex) {
            self.registered.remove(&key);
        }
        self.data.remove(&(ex, scrip));
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn registrations(&self) -> usize {
        self.registered.len()
    }

    pub fn get(&self, exchange: &str, scrip: i32) -> Option<&ScripData> {
        self.data.get(&(exchange.to_ascii_uppercase(), scrip))
    }

    /// Apply a frame. Returns the `(exchange, scrip, kind)` of every packet
    /// that changed stored data. The first binary frame authenticates.
    pub fn apply(&mut self, frame: &[u8]) -> Vec<(String, i32, u8)> {
        self.authenticated = true;
        let mut changed = Vec::new();
        for p in packets(frame) {
            let ex = match self.registered.get(&(p.exchange, p.scrip)) {
                Some(e) => e.clone(),
                None if self.accept_unregistered => exchange_from_char(p.exchange),
                None => continue,
            };
            if !matches!(p.kind, b'A'..=b'H' | b'm' | b'W') {
                // Heartbeat ('1') and undocumented supplementary types.
                continue;
            }
            let b = &p.body;
            let d = self.data.entry((ex.clone(), p.scrip)).or_default();
            match p.kind {
                b'A' => {
                    d.ltp = r2(f32_at(b, 0));
                    d.ltq = i64::from(i32_at(b, 4));
                    d.volume = i64::from(i32_at(b, 8));
                    d.avg_price = r2(f32_at(b, 12));
                    d.oi = i64::from(i32_at(b, 16));
                    d.has_ltp = true;
                }
                b'B'..=b'F' => {
                    let i = usize::from(p.kind - b'B');
                    d.bids[i] = Some(DepthLevel {
                        price: r2(f32_at(b, 0)),
                        quantity: i64::from(i32_at(b, 4)),
                        orders: i64::from(i16_at(b, 8)),
                    });
                    d.asks[i] = Some(DepthLevel {
                        price: r2(f32_at(b, 10)),
                        quantity: i64::from(i32_at(b, 14)),
                        orders: i64::from(i16_at(b, 18)),
                    });
                }
                b'G' => {
                    d.open = r2(f32_at(b, 0));
                    d.high = r2(f32_at(b, 4));
                    d.low = r2(f32_at(b, 8));
                    d.prev_close = r2(f32_at(b, 12));
                    d.has_ohlc = true;
                }
                b'H' => {
                    let v = r2(f32_at(b, 0));
                    d.ltp = v;
                    d.index_rate = Some(v);
                }
                b'm' => {
                    d.oi = i64::from(i32_at(b, 0));
                    d.has_oi = true;
                }
                b'W' => {
                    d.upper_circuit = r2(f32_at(b, 0));
                    d.lower_circuit = r2(f32_at(b, 4));
                }
                _ => continue,
            }
            changed.push((ex, p.scrip, p.kind));
        }
        changed
    }

    pub fn authenticated(&self) -> bool {
        self.authenticated
    }
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
    /// Registered Motilal exchange.
    mo_exchange: String,
}

/// The broadcast feed for the shared `WebSocketManager`.
pub struct MotilalFeed {
    url: String,
    client_code: String,
    state: FeedState,
    subs: HashMap<(String, i32), SubInfo>,
    announced: bool,
}

impl MotilalFeed {
    pub fn new(url: &str, client_code: &str) -> Self {
        Self {
            url: url.to_string(),
            client_code: client_code.to_string(),
            state: FeedState::default(),
            subs: HashMap::new(),
            announced: false,
        }
    }

    /// Instruments currently registered (tests and hygiene).
    pub fn registered(&self) -> usize {
        self.subs.len()
    }

    /// Scrips with stored data (bounded by the registrations).
    pub fn cached(&self) -> usize {
        self.state.len()
    }

    fn frames(&mut self, subs: &[FeedSubscription], add: bool) -> Vec<Message> {
        let mut out = Vec::new();
        for s in subs {
            let Ok(scrip) = s.token.trim().parse::<i32>() else {
                tracing::warn!("Motilal scrip code {} is not numeric; skipped", s.token);
                continue;
            };
            let ex = feed_exchange(&s.exchange).to_string();
            let segment = mapping::segment(&s.exchange);
            let key = (ex.clone(), scrip);
            if add {
                self.state.register(&ex, scrip);
                self.subs.insert(
                    key,
                    SubInfo {
                        symbol: s.symbol.clone(),
                        exchange: s.exchange.clone(),
                        mode: s.mode,
                        mo_exchange: ex.clone(),
                    },
                );
            } else {
                self.state.unregister(&ex, scrip);
                self.subs.remove(&key);
            }
            out.push(Message::Binary(register_packet(&ex, segment, scrip, add)));
        }
        out
    }

    fn events(&self, ex: &str, scrip: i32, kind: u8) -> Vec<FeedEvent> {
        let Some(sub) = self.subs.get(&(ex.to_string(), scrip)) else {
            return Vec::new();
        };
        let Some(d) = self.state.get(&sub.mo_exchange, scrip) else {
            return Vec::new();
        };
        let now = now_ms();
        let depth_packet = (b'B'..=b'F').contains(&kind);
        let mut out = Vec::new();
        if !depth_packet || sub.mode == FeedMode::Depth {
            let mut t = NormalizedTick {
                symbol: sub.symbol.clone(),
                exchange: sub.exchange.clone(),
                mode: sub.mode.code(),
                ltp: d.ltp,
                timestamp_ms: now,
                ..Default::default()
            };
            if sub.mode >= FeedMode::Quote {
                t.open = d.open;
                t.high = d.high;
                t.low = d.low;
                t.close = d.prev_close;
                t.volume = d.volume;
                t.average_price = d.avg_price;
                t.last_quantity = d.ltq;
                t.oi = d.oi;
                t.derive_change();
            }
            if !depth_packet {
                out.push(FeedEvent::Tick(t));
            }
        }
        if sub.mode == FeedMode::Depth && (depth_packet || kind == b'A') {
            out.push(FeedEvent::Depth(NormalizedDepth {
                symbol: sub.symbol.clone(),
                exchange: sub.exchange.clone(),
                ltp: d.ltp,
                buy: ScripData::levels(&d.bids),
                sell: ScripData::levels(&d.asks),
                total_buy_quantity: 0,
                total_sell_quantity: 0,
                timestamp_ms: now,
            }));
        }
        out
    }

    /// Decode one binary frame into events.
    pub fn parse_binary(&mut self, frame: &[u8]) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        if !self.announced {
            self.announced = true;
            out.push(FeedEvent::AuthOk);
        }
        for (ex, scrip, kind) in self.state.apply(frame) {
            out.extend(self.events(&ex, scrip, kind));
        }
        out
    }
}

impl BrokerFeed for MotilalFeed {
    fn broker(&self) -> &'static str {
        "motilal"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Motilal Oswal feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // A new socket: nothing is registered on it yet and the login has
        // not been answered.
        self.announced = false;
        vec![Message::Binary(login_packet(&self.client_code))]
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, true)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, false)
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        // Motilal streams every packet type for a registered scrip; a mode
        // change only changes what is reported.
        let ex = feed_exchange(&new.exchange).to_string();
        if let Ok(scrip) = new.token.trim().parse::<i32>() {
            if let Some(s) = self.subs.get_mut(&(ex, scrip)) {
                s.mode = new.mode;
                return Vec::new();
            }
        }
        let mut v = self.unsubscribe_frames(std::slice::from_ref(old));
        v.extend(self.subscribe_frames(std::slice::from_ref(new)));
        v
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<Value>(t) {
                    if vs(&v, "status").as_deref() == Some("ERROR") {
                        tracing::warn!(
                            "Motilal Oswal market data feed error: {}",
                            vs(&v, "message").unwrap_or_default()
                        );
                    }
                }
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
// Order stream
// ---------------------------------------------------------------------------

/// web `_STATUS_MAP` of the order adapter (same table as the books).
pub fn order_status(status: &str) -> &'static str {
    mapping::map_order_status(status)
}

pub struct MotilalOrderFeed {
    url: String,
    client_code: String,
    auth_token: Secret,
    api_key: Secret,
    symbols: SymbolResolver,
}

impl MotilalOrderFeed {
    pub fn new(url: &str, s: &MotilalSession, symbols: SymbolResolver) -> Self {
        Self {
            url: url.to_string(),
            client_code: s.client_code.clone(),
            auth_token: s.auth_token.clone(),
            api_key: s.api_key.clone(),
            symbols,
        }
    }

    /// web `MotilalOrderUpdateAdapter.normalize`.
    pub fn normalize(&self, text: &str) -> Option<OrderUpdate> {
        let v: Value = serde_json::from_str(text).ok()?;
        if !v.is_object() {
            return None;
        }
        if let Some(code) = vs(&v, "errorcode") {
            tracing::warn!(
                "Motilal Oswal order stream error {}: {}",
                code,
                vs(&v, "message").unwrap_or_default()
            );
            return None;
        }
        if v.get("tradeno").is_some() && v.get("orderstatus").is_none() {
            return None;
        }
        let orderid = vs(&v, "uniqueorderid")?;
        let raw_status = vs(&v, "orderstatus")?;
        let exchange =
            mapping::reverse_map_exchange(&vs(&v, "exchange").unwrap_or_default()).to_string();
        let token = token_text(&v, "symboltoken");
        let symbol = self
            .symbols
            .by_token(&exchange, &token)
            .map(|r| r.symbol)
            .unwrap_or_else(|| vs(&v, "symbol").unwrap_or_default());
        let status = order_status(&raw_status);
        let price = vf(&v, "price");
        let mut pricetype = vs(&v, "ordertype").unwrap_or_default().to_ascii_uppercase();
        if pricetype == "STOPLOSS" {
            pricetype = if price > 0.0 { "SL" } else { "SL-M" }.into();
        }
        let quantity = vi(&v, "orderqty");
        let filled = match vi(&v, "qtytradedtoday") {
            0 => vi(&v, "totalqtytraded"),
            n => n,
        };
        let pending = if v.get("totalqtyremaining").is_some() {
            vi(&v, "totalqtyremaining")
        } else {
            (quantity - filled).max(0)
        };
        Some(OrderUpdate {
            orderid,
            symbol,
            action: vs(&v, "buyorsell").unwrap_or_default().to_ascii_uppercase(),
            quantity,
            price,
            trigger_price: vf(&v, "triggerprice"),
            pricetype,
            product: mapping::reverse_map_product_type(&vs(&v, "producttype").unwrap_or_default())
                .to_string(),
            order_status: status.to_string(),
            filled_quantity: filled,
            pending_quantity: pending,
            average_price: vf(&v, "averageprice"),
            rejection_reason: if status == "rejected" {
                vs(&v, "error").unwrap_or_default()
            } else {
                String::new()
            },
            exchange,
        })
    }
}

impl BrokerFeed for MotilalOrderFeed {
    fn broker(&self) -> &'static str {
        "motilal"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Motilal Oswal order stream address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        vec![
            Message::Text(
                json!({
                    "clientid": self.client_code,
                    "authtoken": self.auth_token.expose(),
                    "apikey": self.api_key.expose(),
                })
                .to_string(),
            ),
            Message::Text(
                json!({"clientid": self.client_code, "action": "OrderSubscribe"}).to_string(),
            ),
        ]
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<Value>(t) {
                    if vs(&v, "errorcode").as_deref() == Some("MO1001") {
                        return vec![FeedEvent::AuthFailed(
                            "Motilal Oswal did not accept the session for order updates. Log in to Motilal Oswal again."
                                .into(),
                        )];
                    }
                }
                self.normalize(t)
                    .map(|u| vec![FeedEvent::OrderUpdate(u)])
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((
            ORDER_HEARTBEAT,
            Message::Text(json!({"clientid": self.client_code, "action": "heartbeat"}).to_string()),
        ))
    }
}
