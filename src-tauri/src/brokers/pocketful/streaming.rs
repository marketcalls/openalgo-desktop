//! Pocketful market-data socket (web `streaming/pocketful_adapter.py`,
//! `streaming/pocketful_mapping.py`, `api/packet_decoder.py`, all taken from
//! Pocketful's Python SDK 0.1.2).
//!
//! * URL `<host>/ws/v1/feeds?login_id=<client_id>&access_token=<token>`
//!   (`packet_decoder.feed_url`); the token rides in the query, so the URL
//!   is never logged and errors are reported by kind only.
//! * Subscribe `{"a":"subscribe","v":[[exchange_code, token], ...],"m":<channel>}`,
//!   batched per (channel, exchange code) like the web; unsubscribe one
//!   instrument per frame with `"a":"unsubscribe"`. Channels: `marketdata`
//!   (detailed, mode 1), `compact_marketdata` (mode 2), `full_snapquote`
//!   (mode 4). OpenAlgo LTP -> compact, Quote -> detailed (web
//!   `MODE_CHANNELS`). Depth -> snapquote with a price stream beside it: a
//!   snapquote has no trade price (the web takes it from the compact
//!   channel; the desktop from the detailed one, R107-07, because it keeps
//!   one stream per instrument at the highest mode held, so a Depth stream
//!   must also serve the Quote and LTP holders of the instrument).
//! * Heartbeat: text `{"a":"h","v":[],"m":""}` every 8 s (the SDK's). The
//!   server answers it, so a socket can stay "alive" with no market data:
//!   once data has been seen in three 30 s buckets within 5 minutes, 90 s
//!   without market data closes the socket and reconnects (web
//!   data-silence watchdog).
//! * Binary frames are big-endian; byte 0 is the mode. Prices are integer
//!   paise (/100); quantities raw. Offsets below cite `packet_decoder.py`.
//!   A text frame carries no packet (web `packet_mode` is None for one).
//! * Order updates come on a socket of their own (`order_feed`): the market
//!   socket publishes market packets only, as the web's adapter does.
//! * A packet is matched to its subscription by token and exchange code
//!   (web `_routes`); the tick is reported under the subscription's
//!   OpenAlgo symbol and exchange.

pub mod order_feed;
#[cfg(test)]
mod tests;

use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, QuoteField, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// web `HEARTBEAT_SECONDS` (the SDK's `heartbeat_thread`).
pub const HEARTBEAT_SECS: u64 = 8;

/// web `HEARTBEAT_MESSAGE`, `json.dumps({"a": "h", "v": [], "m": ""})`
/// without its spaces, in the SDK's key order.
const HEARTBEAT_TEXT: &str = r#"{"a":"h","v":[],"m":""}"#;

/// Pocketful packet modes (byte 0).
pub const MODE_DETAILED: u8 = 1;
pub const MODE_COMPACT: u8 = 2;
pub const MODE_SNAPQUOTE: u8 = 4;

/// The feed's exchange code for an OpenAlgo exchange (web
/// `PocketfulExchangeMapper.EXCHANGE_TYPES`), or none: a token sent on the
/// wrong segment names another instrument, so an exchange the feed does
/// not have is never sent as NSE.
pub(crate) fn feed_exchange_code(exchange: &str) -> Option<u8> {
    Some(match exchange.trim().to_ascii_uppercase().as_str() {
        "NSE" | "NSE_INDEX" => 1,
        "NFO" => 2,
        "CDS" => 3,
        "MCX" => 4,
        "BSE" | "BSE_INDEX" => 6,
        "BFO" => 7,
        _ => return None,
    })
}

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

/// The Pocketful streams an OpenAlgo mode runs: its own, and for Depth the
/// detailed stream first, whose packets carry the trade price the snapquote
/// lacks (R107-07). The web's Depth takes that price from the compact
/// channel instead (`MODE_CHANNELS[3]`); the detailed channel carries the
/// same price and the quote a Quote holder of the instrument needs, which
/// the desktop's one stream per instrument must serve too.
fn streams(mode: FeedMode) -> &'static [u8] {
    match pocketful_mode(mode) {
        MODE_SNAPQUOTE => &[MODE_DETAILED, MODE_SNAPQUOTE],
        MODE_COMPACT => &[MODE_COMPACT],
        _ => &[MODE_DETAILED],
    }
}

/// One subscribe or unsubscribe frame.
pub fn sub_frame(action: &str, code: u8, token: u32, pocketful_mode: u8) -> Message {
    Message::Text(
        json!({"a": action, "v": [[code, token]], "m": market_type(pocketful_mode)}).to_string(),
    )
}

/// (Pocketful mode, exchange code): one subscribe frame per key.
type BatchKey = (u8, u8);

/// One subscribe frame for several instruments of one type
/// (web `_send_subscription_batch`).
pub fn batch_frame(action: &str, pocketful_mode: u8, instruments: &[(u8, u32)]) -> Message {
    let v: Vec<Value> = instruments.iter().map(|(c, t)| json!([c, t])).collect();
    Message::Text(json!({"a": action, "v": v, "m": market_type(pocketful_mode)}).to_string())
}

/// Seconds per activity bucket, buckets needed and the window they must
/// fall in before the watchdog is active, and the silence that then
/// triggers a reconnect (web `DATA_ARM_*`, `DATA_SILENCE_TIMEOUT`).
pub const DATA_ARM_BUCKET_SECS: u64 = 30;
pub const DATA_ARM_BUCKETS: usize = 3;
pub const DATA_ARM_WINDOW_SECS: u64 = 300;
pub const DATA_SILENCE: Duration = Duration::from_secs(90);

/// Market-data silence watchdog (web `_record_market_data`,
/// `_health_check_loop`). It only becomes active once data has flowed in
/// three distinct 30 s buckets within 5 minutes, so a quiet instrument
/// list or a closed market never triggers reconnects.
#[derive(Debug)]
pub struct DataWatchdog {
    origin: Instant,
    last_data: Option<Instant>,
    armed: bool,
    buckets: VecDeque<u64>,
}

impl DataWatchdog {
    pub fn new(origin: Instant) -> Self {
        Self {
            origin,
            last_data: None,
            armed: false,
            buckets: VecDeque::with_capacity(DATA_ARM_BUCKETS),
        }
    }

    /// A market-data packet arrived at `now`.
    pub fn record(&mut self, now: Instant) {
        self.last_data = Some(now);
        if self.armed {
            return;
        }
        let secs = now.saturating_duration_since(self.origin).as_secs();
        let bucket = secs - secs % DATA_ARM_BUCKET_SECS;
        if self.buckets.back() != Some(&bucket) {
            if self.buckets.len() == DATA_ARM_BUCKETS {
                self.buckets.pop_front();
            }
            self.buckets.push_back(bucket);
        }
        self.buckets
            .retain(|b| secs.saturating_sub(*b) <= DATA_ARM_WINDOW_SECS);
        if self.buckets.len() >= DATA_ARM_BUCKETS {
            self.armed = true;
            tracing::info!(broker = "pocketful", "Market-data silence watchdog active");
        }
    }

    pub fn armed(&self) -> bool {
        self.armed
    }

    /// Active and no market data for `DATA_SILENCE`.
    pub fn stalled(&self, now: Instant) -> bool {
        self.armed
            && self
                .last_data
                .is_some_and(|t| now.saturating_duration_since(t) >= DATA_SILENCE)
    }
}

pub fn heartbeat_frame() -> Message {
    Message::Text(HEARTBEAT_TEXT.into())
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

/// Snapquote (mode 4, at least 166 bytes; `decodeSnapquoteData`). No trade
/// price: `average_price` is the day's average trade price, never a last
/// traded price (R107-07; the web too since a1d539764).
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

/// A decoded market packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    Detailed(Detailed),
    Compact(Compact),
    Snapquote(Snapquote),
}

impl Packet {
    pub fn token(&self) -> Option<(u8, u32)> {
        Some(match self {
            Packet::Detailed(d) => (d.exchange_code, d.token),
            Packet::Compact(c) => (c.exchange_code, c.token),
            Packet::Snapquote(s) => (s.exchange_code, s.token),
        })
    }
}

/// Decode one socket frame (web `_on_message`): a binary packet by its
/// mode byte. A text frame has no mode byte and carries no market data
/// (web `packet_mode` returns None for one).
pub fn decode_frame(msg: &Message) -> Option<Packet> {
    match msg {
        Message::Binary(b) => decode_binary(b),
        _ => None,
    }
}

pub fn decode_binary(b: &[u8]) -> Option<Packet> {
    match *b.first()? {
        MODE_DETAILED => decode_detailed(b).map(Packet::Detailed),
        MODE_COMPACT => decode_compact(b).map(Packet::Compact),
        MODE_SNAPQUOTE => decode_snapquote(b).map(Packet::Snapquote),
        _ => None,
    }
}

/// web `_price`: a price of 0 is one the feed does not have yet (no trade
/// today), left out rather than sent as a price.
fn mark_unpriced(t: &mut NormalizedTick, prices: &[(QuoteField, f64)]) {
    for &(field, value) in prices {
        if value <= 0.0 {
            t.mark_absent(field);
        }
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

/// The packet's trade time; none before the first trade (0, or anything
/// before 2001): no `ltt` then, never the receipt time (the web sends the
/// raw value; lead decision 2026-11-09, review round).
fn set_trade_time(t: &mut NormalizedTick, ltt: u32) {
    t.last_trade_time_ms = ltt_ms(ltt);
    if t.last_trade_time_ms == 0 {
        t.mark_absent(QuoteField::LastTradeTime);
    }
}

// ---------------------------------------------------------------------------
// Feed
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
}

pub struct PocketfulFeed {
    url: String,
    /// (exchange code, token) -> subscription.
    subs: HashMap<(u8, u32), SubInfo>,
    watchdog: DataWatchdog,
}

impl PocketfulFeed {
    /// The session's feed. Packets name their instrument by exchange code
    /// and token, matched to the subscriptions themselves, so the symbol
    /// master (taken for the order feed's sake by the same call in the
    /// session code) is not read here.
    pub fn new(
        ws_base: &str,
        client_id: &str,
        access_token: &str,
        _symbols: SymbolResolver,
    ) -> Self {
        Self {
            url: feed_url(ws_base, client_id, access_token),
            subs: HashMap::new(),
            watchdog: DataWatchdog::new(Instant::now()),
        }
    }

    /// The (exchange code, token) a subscription streams on: the web's
    /// `subscribe` refuses a token that is not a number and an exchange the
    /// feed does not have, and sends neither.
    fn key(s: &FeedSubscription) -> Option<(u8, u32)> {
        let token = s.token.trim().parse::<u32>().ok();
        let code = feed_exchange_code(&s.exchange);
        if token.is_none() || code.is_none() {
            tracing::warn!(
                "Pocketful streams no live prices for {}:{}",
                s.exchange,
                s.symbol
            );
        }
        Some((code?, token?))
    }

    /// One packet of a subscribed instrument -> events, as the web's
    /// packet handlers publish it (`_handle_compact_data`,
    /// `_handle_detailed_data`, `_handle_snapquote_data`).
    fn tick_from(&self, sub: &SubInfo, p: &Packet) -> Vec<FeedEvent> {
        let now = now_ms();
        let t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            timestamp_ms: now,
            ..Default::default()
        };
        let index = sub.exchange.ends_with("_INDEX");
        match p {
            Packet::Compact(c) => vec![FeedEvent::Tick(compact_tick(t, c))],
            Packet::Detailed(d) => vec![FeedEvent::Tick(detailed_tick(t, d, index))],
            Packet::Snapquote(s) => vec![
                FeedEvent::Tick(snapquote_tick(t, s)),
                FeedEvent::Depth(snapquote_book(sub, s, now)),
            ],
        }
    }

    pub fn handle(&self, p: Packet) -> Vec<FeedEvent> {
        let Some(key) = p.token() else {
            return Vec::new();
        };
        match self.subs.get(&key) {
            Some(sub) => self.tick_from(sub, &p),
            None => Vec::new(),
        }
    }
}

/// A compact packet: the LTP frame's price (web `{ltp, timestamp}`, only
/// for a price above 0) and its trade time. The packet's change, OI and
/// best prices are not in the web's LTP frame any more, and the docs' LTP
/// frame has none of them.
fn compact_tick(mut t: NormalizedTick, c: &Compact) -> NormalizedTick {
    t.mode = FeedMode::Ltp.code();
    t.ltp = c.ltp;
    set_trade_time(&mut t, c.ltt);
    mark_unpriced(&mut t, &[(QuoteField::Ltp, c.ltp)]);
    t
}

/// A detailed packet: the Quote frame (web `_handle_detailed_data`), every
/// price above 0, the best bid and ask with their sizes, the OI, and the
/// change against the previous close when both prices are known. An index
/// carries no OI, book or order flow; its zeros are left out.
fn detailed_tick(mut t: NormalizedTick, d: &Detailed, index: bool) -> NormalizedTick {
    t.mode = FeedMode::Quote.code();
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
    set_trade_time(&mut t, d.ltt);
    mark_unpriced(
        &mut t,
        &[
            (QuoteField::Ltp, d.ltp),
            (QuoteField::Open, d.open),
            (QuoteField::High, d.high),
            (QuoteField::Low, d.low),
            (QuoteField::Close, d.close),
            (QuoteField::AveragePrice, d.average_price),
        ],
    );
    if index {
        for f in [
            QuoteField::OpenInterest,
            QuoteField::LastQuantity,
            QuoteField::TotalBuyQuantity,
            QuoteField::TotalSellQuantity,
        ] {
            t.mark_absent(f);
        }
    } else {
        t.mark_carried(QuoteField::OpenInterest);
        t.extras.bid_price = Some(d.bid);
        t.extras.bid_quantity = Some(d.bid_qty);
        t.extras.ask_price = Some(d.ask);
        t.extras.ask_quantity = Some(d.ask_qty);
    }
    t.derive_change();
    if d.ltp <= 0.0 || d.close <= 0.0 {
        t.mark_absent(QuoteField::Change);
    }
    t
}

/// A snapquote's quote: its OHLC and average trade price above 0, volume
/// and book totals, with no trade price, trade time, last quantity or OI
/// (it carries none; R107-07). Knowing no price, it only updates the
/// instrument's snapshot, and its book reaches Depth holders alone with
/// the last trade price the detailed stream brought (`feed::bridge::pairing`).
fn snapquote_tick(mut t: NormalizedTick, s: &Snapquote) -> NormalizedTick {
    t.mode = FeedMode::Depth.code();
    for f in [
        QuoteField::Ltp,
        QuoteField::LastTradeTime,
        QuoteField::LastQuantity,
        QuoteField::OpenInterest,
    ] {
        t.mark_absent(f);
    }
    t.average_price = s.average_price;
    t.open = s.open;
    t.high = s.high;
    t.low = s.low;
    t.close = s.close;
    t.volume = s.volume;
    t.total_buy_quantity = s.total_buy_qty;
    t.total_sell_quantity = s.total_sell_qty;
    mark_unpriced(
        &mut t,
        &[
            (QuoteField::Open, s.open),
            (QuoteField::High, s.high),
            (QuoteField::Low, s.low),
            (QuoteField::Close, s.close),
            (QuoteField::AveragePrice, s.average_price),
        ],
    );
    t
}

/// A snapquote's book, all five levels each side as the packet holds them
/// (web `_levels`), without a price of its own.
fn snapquote_book(sub: &SubInfo, s: &Snapquote, now: i64) -> NormalizedDepth {
    let mut d = NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        buy: s.bids.clone(),
        sell: s.asks.clone(),
        total_buy_quantity: s.total_buy_qty,
        total_sell_quantity: s.total_sell_qty,
        timestamp_ms: now,
        ..Default::default()
    };
    d.absent = d.absent.with(QuoteField::Ltp);
    d
}

/// `<host>/ws/v1/feeds?login_id=..&access_token=..` (web
/// `packet_decoder.feed_url`, the SDKs' address). It carries the access
/// token: never log it.
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

    fn on_connected(&mut self) -> Vec<Message> {
        self.watchdog = DataWatchdog::new(Instant::now());
        Vec::new()
    }

    /// One frame per (Pocketful type, exchange code), instruments in
    /// request order without repeats (web `_process_batch_subscriptions`).
    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut groups: Vec<(BatchKey, Vec<(u8, u32)>)> = Vec::new();
        for s in subs {
            let Some(key) = Self::key(s) else { continue };
            self.subs.insert(
                key,
                SubInfo {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                },
            );
            for &stream in streams(s.mode) {
                let group = (stream, key.0);
                match groups.iter_mut().find(|(g, _)| *g == group) {
                    Some((_, v)) => {
                        if !v.contains(&key) {
                            v.push(key)
                        }
                    }
                    None => groups.push((group, vec![key])),
                }
            }
        }
        groups
            .into_iter()
            .map(|((mode, _), v)| batch_frame("subscribe", mode, &v))
            .collect()
    }

    /// Only the difference between the two modes' streams: Quote and
    /// Depth both run the detailed stream, which stays up between them.
    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        let Some(key) = Self::key(new) else {
            return Vec::new();
        };
        let (before, after) = (streams(old.mode), streams(new.mode));
        let frame = |action, stream: &u8| sub_frame(action, key.0, key.1, *stream);
        let leaving = before.iter().filter(|s| !after.contains(s));
        let arriving = after.iter().filter(|s| !before.contains(s));
        leaving
            .map(|s| frame("unsubscribe", s))
            .chain(arriving.map(|s| frame("subscribe", s)))
            .collect()
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut out = Vec::new();
        for s in subs {
            let Some(key) = Self::key(s) else { continue };
            self.subs.remove(&key);
            out.extend(
                streams(s.mode)
                    .iter()
                    .map(|&stream| sub_frame("unsubscribe", key.0, key.1, stream)),
            );
        }
        out
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match decode_frame(msg) {
            Some(p) => {
                self.watchdog.record(Instant::now());
                self.handle(p)
            }
            None => Vec::new(),
        }
    }

    fn data_stalled(&mut self) -> bool {
        if !self.watchdog.stalled(Instant::now()) {
            return false;
        }
        tracing::warn!(
            broker = "pocketful",
            "No market data for {} s; reconnecting",
            DATA_SILENCE.as_secs()
        );
        true
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((Duration::from_secs(HEARTBEAT_SECS), heartbeat_frame()))
    }
}
