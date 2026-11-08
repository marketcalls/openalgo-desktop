//! Delta public market-data socket (web `streaming/delta_websocket.py`,
//! `delta_adapter.py`, `delta_mapping.py`).
//!
//! * `wss://public-socket.india.delta.exchange`, no authentication (the
//!   private socket's `key-auth` frame is refused there).
//! * Frames are JSON: `{"type":"subscribe"|"unsubscribe","payload":
//!   {"channels":[{"name":"ticker","symbols":[..]}]}}`.
//! * Modes: LTP and QUOTE subscribe `ticker`; DEPTH subscribes `ob_l2`
//!   and `ticker` (the book carries no prices or OI). `ticker` takes any
//!   number of symbols per frame; `ob_l2` exactly one.
//! * `ticker`: `{"type":"ticker","sy","sp","d":[{"s","m","ohlc":[o,h,l,c],
//!   "oi":[oi,chg],"q":[ask,ask_size,bid,bid_size,mid]}]}`; LTP is the mark
//!   price `m` (else the frame's spot `sp`). A field Delta omits keeps its
//!   last value; a null inside a present array publishes 0.
//! * `ob_l2`: `{"type":"ob_l2","sy","a":[[price,size],..],"b":[..]}`, best
//!   first, top five used.
//! * Fields from both channels are merged per symbol, so a depth
//!   subscriber still carries LTP and OI, and every ticker sends a Depth
//!   subscriber the book again with the new prices (web `_on_data`
//!   publishes the merged snapshot on either channel, audit P-07). The
//!   caches hold one entry per subscribed symbol and are cleared on
//!   unsubscribe.
//! * The ticker's best bid and ask (`q`) go out as the web's `bid_price`,
//!   `bid_qty`, `ask_price`, `ask_qty` (parity, replacing decision BF-02);
//!   a null inside a present `q` is 0, as on the web.
//! * What the ticker never carries (volume, average price, last quantity,
//!   book totals, a trade time) is left out, never sent as 0; a book has
//!   no order counts and is not padded.
//! * Not carried: Delta's private channels (orders, positions, margins),
//!   which the web opens but its proxy drops (no `user_id`), so no web
//!   client receives them either.

use crate::brokers::common::streaming::{
    now_ms, AbsentFields, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
    NormalizedDepth, NormalizedTick, QuoteField, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const CHANNEL_TICKER: &str = "ticker";
pub const CHANNEL_OB_L2: &str = "ob_l2";
/// web `HEARTBEAT_INTERVAL` (protocol pings).
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct Sub {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct DeltaFeed {
    url: String,
    /// Broker symbol -> subscription.
    subs: HashMap<String, Sub>,
    /// Broker symbol -> last merged ticker values.
    cache: HashMap<String, NormalizedTick>,
    /// Broker symbol -> last book (bids, asks), for the Depth frame a
    /// ticker refreshes.
    books: HashMap<String, Book>,
}

type Book = (Vec<DepthLevel>, Vec<DepthLevel>);

/// What Delta's ticker never carries.
const NOT_IN_TICKER: AbsentFields = AbsentFields::of(&[
    QuoteField::Volume,
    QuoteField::AveragePrice,
    QuoteField::LastQuantity,
    QuoteField::TotalBuyQuantity,
    QuoteField::TotalSellQuantity,
    QuoteField::LastTradeTime,
]);

/// What a symbol's first ticker may not have sent yet.
const NOT_YET_SENT: AbsentFields = AbsentFields::of(&[
    QuoteField::Ltp,
    QuoteField::Open,
    QuoteField::High,
    QuoteField::Low,
    QuoteField::Close,
    QuoteField::OpenInterest,
]);

impl DeltaFeed {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            subs: HashMap::new(),
            cache: HashMap::new(),
            books: HashMap::new(),
        }
    }

    /// Number of instruments with cached values (bounded by subscriptions).
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    fn frame(kind: &str, channel: &str, symbols: &[String]) -> Message {
        Message::Text(
            json!({
                "type": kind,
                "payload": {"channels": [{"name": channel, "symbols": symbols}]}
            })
            .to_string(),
        )
    }

    /// One `ticker` frame for all symbols, one `ob_l2` frame per depth
    /// symbol (Delta refuses more than one there).
    fn frames(kind: &str, ticker: &[String], depth: &[String]) -> Vec<Message> {
        let mut out = Vec::new();
        if !ticker.is_empty() {
            out.push(Self::frame(kind, CHANNEL_TICKER, ticker));
        }
        for s in depth {
            out.push(Self::frame(kind, CHANNEL_OB_L2, std::slice::from_ref(s)));
        }
        out
    }

    fn ticker(&mut self, v: &Value) -> Vec<FeedEvent> {
        let spot = v.get("sp");
        let Some(entries) = v.get("d").and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let now = now_ms();
        for e in entries {
            let br = e
                .get("s")
                .and_then(Value::as_str)
                .or_else(|| v.get("sy").and_then(Value::as_str))
                .unwrap_or("");
            let Some(sub) = self.subs.get(br) else {
                continue;
            };
            let t = self
                .cache
                .entry(br.to_string())
                .or_insert_with(|| NormalizedTick {
                    symbol: sub.symbol.clone(),
                    exchange: sub.exchange.clone(),
                    absent: NOT_IN_TICKER.union(NOT_YET_SENT),
                    ..Default::default()
                });
            merge_ticker(t, e, spot);
            t.timestamp_ms = now;
            let tick = t.clone();
            out.push(FeedEvent::Tick(tick.clone()));
            if sub.mode == FeedMode::Depth {
                if let Some((buy, sell)) = self.books.get(br) {
                    out.push(FeedEvent::Depth(book_event(sub, &tick, buy, sell, now)));
                }
            }
        }
        out
    }

    fn book(&mut self, v: &Value) -> Vec<FeedEvent> {
        let br = v.get("sy").and_then(Value::as_str).unwrap_or("");
        let Some(sub) = self.subs.get(br) else {
            return Vec::new();
        };
        let side = |k: &str| -> Vec<DepthLevel> {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_array)
                        .filter(|lvl| lvl.len() >= 2)
                        .take(5)
                        .map(|lvl| DepthLevel {
                            price: at(lvl, 0),
                            quantity: at(lvl, 1) as i64,
                            orders: 0,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let (buy, sell) = (side("b"), side("a"));
        let tick = self.cache.get(br).cloned().unwrap_or_default();
        let depth = book_event(sub, &tick, &buy, &sell, now_ms());
        self.books.insert(br.to_string(), (buy, sell));
        vec![FeedEvent::Depth(depth)]
    }

    /// Decode one text frame.
    pub fn parse_text(&mut self, text: &str) -> Vec<FeedEvent> {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return Vec::new();
        };
        match v.get("type").and_then(Value::as_str) {
            Some(CHANNEL_TICKER) => self.ticker(&v),
            Some(CHANNEL_OB_L2) => self.book(&v),
            Some("heartbeat") | Some("subscriptions") => vec![FeedEvent::Heartbeat],
            Some("error") => {
                let detail = v
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| v.get("error").and_then(Value::as_str))
                    .unwrap_or("");
                tracing::warn!("Delta Exchange feed error: {}", detail);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

/// Fold one ticker entry into the symbol's cached values. A field Delta
/// omits keeps its last value; a present array is authoritative (web
/// `_put_from`). The ticker is a Quote packet that also carries OI.
fn merge_ticker(t: &mut NormalizedTick, e: &Value, spot: Option<&Value>) {
    if let Some(ltp) = e.get("m").filter(|m| !m.is_null()).or(spot).and_then(num) {
        t.ltp = ltp;
        t.absent = t.absent.without(QuoteField::Ltp);
    }
    if let Some(ohlc) = e.get("ohlc").and_then(Value::as_array) {
        t.open = at(ohlc, 0);
        t.high = at(ohlc, 1);
        t.low = at(ohlc, 2);
        t.close = at(ohlc, 3);
        for f in [
            QuoteField::Open,
            QuoteField::High,
            QuoteField::Low,
            QuoteField::Close,
        ] {
            t.absent = t.absent.without(f);
        }
    }
    if let Some(oi) = e.get("oi").and_then(Value::as_array) {
        t.oi = at(oi, 0) as i64;
        t.absent = t.absent.without(QuoteField::OpenInterest);
    }
    if let Some(q) = e.get("q").and_then(Value::as_array) {
        t.extras.bid_price = Some(at(q, 2));
        t.extras.bid_quantity = Some(at(q, 3) as i64);
        t.extras.ask_price = Some(at(q, 0));
        t.extras.ask_quantity = Some(at(q, 1) as i64);
    }
    t.mode = FeedMode::Quote.code();
    t.carried = AbsentFields::of(&[QuoteField::OpenInterest]);
    t.derive_change();
}

/// The book of `sub` with the ticker's price: the levels Delta sent (no
/// order counts, no padding) and their sums as the totals, the web's
/// `totalbuyqty` / `totalsellqty`.
fn book_event(
    sub: &Sub,
    tick: &NormalizedTick,
    buy: &[DepthLevel],
    sell: &[DepthLevel],
    now: i64,
) -> NormalizedDepth {
    let mut absent = AbsentFields::of(&[QuoteField::Orders]);
    if tick.absent.contains(QuoteField::Ltp) || tick.ltp <= 0.0 {
        absent = absent.with(QuoteField::Ltp);
    }
    NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        ltp: tick.ltp,
        total_buy_quantity: buy.iter().map(|l| l.quantity).sum(),
        total_sell_quantity: sell.iter().map(|l| l.quantity).sum(),
        buy: buy.to_vec(),
        sell: sell.to_vec(),
        timestamp_ms: now,
        absent,
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Element `i` of a present array; missing or null publishes 0.
fn at(a: &[Value], i: usize) -> f64 {
    a.get(i).and_then(num).unwrap_or(0.0)
}

impl BrokerFeed for DeltaFeed {
    fn broker(&self) -> &'static str {
        "deltaexchange"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Delta Exchange feed address is invalid".into()))
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut ticker = Vec::new();
        let mut depth = Vec::new();
        for s in subs {
            let br = s.brsymbol.clone();
            self.subs.insert(
                br.clone(),
                Sub {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    mode: s.mode,
                },
            );
            if s.mode == FeedMode::Depth {
                depth.push(br.clone());
            }
            ticker.push(br);
        }
        Self::frames("subscribe", &ticker, &depth)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut ticker = Vec::new();
        let mut depth = Vec::new();
        for s in subs {
            self.subs.remove(&s.brsymbol);
            self.cache.remove(&s.brsymbol);
            self.books.remove(&s.brsymbol);
            if s.mode == FeedMode::Depth {
                depth.push(s.brsymbol.clone());
            }
            ticker.push(s.brsymbol.clone());
        }
        Self::frames("unsubscribe", &ticker, &depth)
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        if let Some(s) = self.subs.get_mut(&new.brsymbol) {
            s.mode = new.mode;
        }
        let br = std::slice::from_ref(&new.brsymbol);
        // The ticker stays subscribed in every mode; only the book moves.
        match (old.mode == FeedMode::Depth, new.mode == FeedMode::Depth) {
            (false, true) => vec![Self::frame("subscribe", CHANNEL_OB_L2, br)],
            (true, false) => vec![Self::frame("unsubscribe", CHANNEL_OB_L2, br)],
            _ => Vec::new(),
        }
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_text(t),
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
