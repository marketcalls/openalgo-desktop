//! The 50-level TBT depth socket (web `fyers_tbt_websocket.py`): its
//! protobuf messages, the order books it keeps and the address lookup
//! before each connect.

use super::request_with_auth;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedSubscription, Message, NormalizedDepth, PrepareError,
    WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{AuthToken, DepthLevel};
use crate::error::Result;
use crate::security::Secret;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

/// Minimal protobuf reader for `msg.proto` (wire types 0, 1, 2, 5).
pub mod proto {
    /// One decoded field.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum Field<'a> {
        Varint(u64),
        Bytes(&'a [u8]),
        Fixed64(u64),
        Fixed32(u32),
    }

    pub fn varint(b: &[u8], pos: &mut usize) -> Option<u64> {
        let mut v: u64 = 0;
        for shift in (0..64).step_by(7) {
            let byte = *b.get(*pos)?;
            *pos += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// All `(field number, value)` pairs of a message; `None` if malformed.
    pub fn fields(b: &[u8]) -> Option<Vec<(u32, Field<'_>)>> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < b.len() {
            let key = varint(b, &mut pos)?;
            let (num, wire) = ((key >> 3) as u32, key & 7);
            let f = match wire {
                0 => Field::Varint(varint(b, &mut pos)?),
                1 => {
                    let s = b.get(pos..pos + 8)?;
                    pos += 8;
                    Field::Fixed64(u64::from_le_bytes(s.try_into().ok()?))
                }
                2 => {
                    let len = varint(b, &mut pos)? as usize;
                    let s = b.get(pos..pos.checked_add(len)?)?;
                    pos += len;
                    Field::Bytes(s)
                }
                5 => {
                    let s = b.get(pos..pos + 4)?;
                    pos += 4;
                    Field::Fixed32(u32::from_le_bytes(s.try_into().ok()?))
                }
                _ => return None,
            };
            out.push((num, f));
        }
        Some(out)
    }

    /// Value of a `google.protobuf.*Value` wrapper (absent field = 0).
    pub fn wrapper(b: &[u8]) -> u64 {
        fields(b)
            .and_then(|fs| {
                fs.into_iter().find_map(|(n, f)| match (n, f) {
                    (1, Field::Varint(v)) => Some(v),
                    _ => None,
                })
            })
            .unwrap_or(0)
    }
}

/// One `MarketLevel` update; `None` fields were absent on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LevelUpdate {
    pub price: Option<i64>,
    pub qty: Option<u64>,
    pub nord: Option<u64>,
    pub num: Option<u64>,
}

/// One `MarketFeed` with depth.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TbtDepthUpdate {
    pub ticker: String,
    pub token: String,
    pub tbq: Option<u64>,
    pub tsq: Option<u64>,
    pub bids: Vec<LevelUpdate>,
    pub asks: Vec<LevelUpdate>,
    pub feed_time: u64,
}

/// A decoded `SocketMessage`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TbtMessage {
    pub snapshot: bool,
    pub error: bool,
    pub msg: String,
    pub feeds: Vec<TbtDepthUpdate>,
}

fn level(b: &[u8]) -> Option<LevelUpdate> {
    use proto::Field;
    let mut l = LevelUpdate::default();
    for (n, f) in proto::fields(b)? {
        if let Field::Bytes(w) = f {
            let v = proto::wrapper(w);
            match n {
                1 => l.price = Some(v as i64),
                2 => l.qty = Some(v),
                3 => l.nord = Some(v),
                4 => l.num = Some(v),
                _ => {}
            }
        }
    }
    Some(l)
}

fn market_feed(key: &str, b: &[u8]) -> Option<Option<TbtDepthUpdate>> {
    use proto::Field;
    let mut u = TbtDepthUpdate {
        token: key.to_string(),
        ..Default::default()
    };
    let mut has_depth = false;
    for (n, f) in proto::fields(b)? {
        match (n, f) {
            (5, Field::Bytes(d)) => {
                has_depth = true;
                for (dn, df) in proto::fields(d)? {
                    match (dn, df) {
                        (1, Field::Bytes(w)) => u.tbq = Some(proto::wrapper(w)),
                        (2, Field::Bytes(w)) => u.tsq = Some(proto::wrapper(w)),
                        (3, Field::Bytes(l)) => u.asks.push(level(l)?),
                        (4, Field::Bytes(l)) => u.bids.push(level(l)?),
                        _ => {}
                    }
                }
            }
            (6, Field::Bytes(w)) => u.feed_time = proto::wrapper(w),
            (11, Field::Bytes(t)) => u.ticker = String::from_utf8_lossy(t).into_owned(),
            _ => {}
        }
    }
    if u.ticker.is_empty() {
        u.ticker = u.token.clone();
    }
    Some(has_depth.then_some(u))
}

/// Decode a TBT `SocketMessage` (feeds without depth are dropped).
pub fn decode_tbt(b: &[u8]) -> Option<TbtMessage> {
    use proto::Field;
    let mut m = TbtMessage::default();
    for (n, f) in proto::fields(b)? {
        match (n, f) {
            (2, Field::Bytes(entry)) => {
                let mut key = String::new();
                let mut value: Option<&[u8]> = None;
                for (en, ef) in proto::fields(entry)? {
                    match (en, ef) {
                        (1, Field::Bytes(k)) => key = String::from_utf8_lossy(k).into_owned(),
                        (2, Field::Bytes(v)) => value = Some(v),
                        _ => {}
                    }
                }
                if let Some(v) = value {
                    if let Some(u) = market_feed(&key, v)? {
                        m.feeds.push(u);
                    }
                }
            }
            (3, Field::Varint(v)) => m.snapshot = v != 0,
            (4, Field::Bytes(s)) => m.msg = String::from_utf8_lossy(s).into_owned(),
            (5, Field::Varint(v)) => m.error = v != 0,
            _ => {}
        }
    }
    Some(m)
}

/// Exchanges with 50-level depth (web `TBT_SUPPORTED_EXCHANGES`).
pub const TBT_EXCHANGES: &[&str] = &["NSE", "NFO"];
pub const TBT_LEVELS: usize = 50;

#[derive(Debug, Clone)]
struct Book {
    buy: Vec<DepthLevel>,
    sell: Vec<DepthLevel>,
    tbq: i64,
    tsq: i64,
}

impl Book {
    fn empty() -> Self {
        Self {
            buy: vec![DepthLevel::default(); TBT_LEVELS],
            sell: vec![DepthLevel::default(); TBT_LEVELS],
            tbq: 0,
            tsq: 0,
        }
    }
}

/// Where to look up the TBT socket address before connecting.
struct TbtLookup {
    http: reqwest::Client,
    url: String,
}

/// The TBT 50-level depth feed.
pub struct TbtFeed {
    url: String,
    lookup: Option<TbtLookup>,
    authorization: Secret,
    /// ticker -> (symbol, exchange)
    subs: HashMap<String, (String, String)>,
    /// ticker -> book (one per subscription, removed with it)
    books: HashMap<String, Book>,
    channel_resumed: bool,
}

impl TbtFeed {
    pub fn new(url: &str, auth: &AuthToken, _symbols: SymbolResolver) -> Result<Self> {
        auth.pair().ok_or_else(super::super::session_expired)?;
        Ok(Self {
            url: url.to_string(),
            lookup: None,
            authorization: Secret::new(auth.raw()),
            subs: HashMap::new(),
            books: HashMap::new(),
            channel_resumed: false,
        })
    }

    /// Ask Fyers for the socket address before each connect (web
    /// `_get_tbt_url`); `url` stays the fallback.
    pub fn with_lookup(mut self, http: reqwest::Client, lookup_url: String) -> Self {
        self.lookup = Some(TbtLookup {
            http,
            url: lookup_url,
        });
        self
    }

    fn sub_frame(tickers: &[String], subs: i32) -> Message {
        Message::Text(
            json!({
                "type": 1,
                "data": {"subs": subs, "symbols": tickers, "mode": "depth", "channel": "1"}
            })
            .to_string(),
        )
    }

    fn switch_frame() -> Message {
        Message::Text(
            json!({"type": 2, "data": {"resumeChannels": ["1"], "pauseChannels": []}}).to_string(),
        )
    }

    /// web `_extract_depth`: a snapshot resets the book; levels are placed
    /// by their `num`; prices are paise.
    fn apply(&mut self, u: &TbtDepthUpdate, snapshot: bool) -> Option<NormalizedDepth> {
        let (symbol, exchange) = self.subs.get(&u.ticker)?.clone();
        let book = self
            .books
            .entry(u.ticker.clone())
            .or_insert_with(Book::empty);
        if snapshot {
            book.buy = vec![DepthLevel::default(); TBT_LEVELS];
            book.sell = vec![DepthLevel::default(); TBT_LEVELS];
        }
        let put = |side: &mut Vec<DepthLevel>, l: &LevelUpdate| {
            let Some(idx) = l.num.map(|n| n as usize).filter(|n| *n < TBT_LEVELS) else {
                return;
            };
            if let Some(p) = l.price {
                side[idx].price = p as f64 / 100.0;
            }
            if let Some(q) = l.qty {
                side[idx].quantity = q as i64;
            }
            if let Some(o) = l.nord {
                side[idx].orders = o as i64;
            }
        };
        for l in &u.bids {
            put(&mut book.buy, l);
        }
        for l in &u.asks {
            put(&mut book.sell, l);
        }
        if let Some(v) = u.tbq {
            book.tbq = v as i64;
        }
        if let Some(v) = u.tsq {
            book.tsq = v as i64;
        }
        let buy: Vec<DepthLevel> = book.buy.iter().filter(|l| l.price > 0.0).copied().collect();
        let sell: Vec<DepthLevel> = book
            .sell
            .iter()
            .filter(|l| l.price > 0.0)
            .copied()
            .collect();
        // TBT depth has no LTP, so none is set (0): the bridge gives the
        // frame the quote feed's last traded price, never the bid/ask
        // midpoint the web used to invent (web #2318,
        // `fyers_websocket_adapter.py` `get_last_ltp`).
        Some(NormalizedDepth {
            symbol,
            exchange,
            ltp: 0.0,
            buy,
            sell,
            total_buy_quantity: book.tbq,
            total_sell_quantity: book.tsq,
            timestamp_ms: now_ms(),
        })
    }
}

#[async_trait::async_trait]
impl BrokerFeed for TbtFeed {
    fn broker(&self) -> &'static str {
        "fyers"
    }

    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        let Some(l) = &self.lookup else {
            return Ok(());
        };
        let resp = l
            .http
            .get(&l.url)
            .header("Authorization", self.authorization.expose())
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().as_u16() == 401 => return Err(PrepareError::AuthFailed(
                "Fyers did not accept the stored login for market depth. Log in to Fyers again."
                    .into(),
            )),
            Ok(r) if r.status().is_success() => {
                if let Ok(v) = r.json::<serde_json::Value>().await {
                    if let Some(u) = v
                        .pointer("/data/socket_url")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| s.starts_with("wss://") || s.starts_with("ws://"))
                    {
                        self.url = u.to_string();
                    }
                }
            }
            Ok(r) => tracing::warn!(
                status = r.status().as_u16(),
                "Fyers depth address lookup refused; using the default"
            ),
            Err(e) => tracing::warn!(
                "Fyers depth address lookup failed: {}",
                crate::brokers::common::redact::url_safe_error(&e)
            ),
        }
        Ok(())
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request_with_auth(&self.url, "authorization", self.authorization.expose())
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.channel_resumed = false;
        self.books.clear();
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut tickers = Vec::new();
        for s in subs {
            if !TBT_EXCHANGES.contains(&s.exchange.as_str()) {
                tracing::warn!(
                    "50-level depth is not available for {}:{}",
                    s.exchange,
                    s.symbol
                );
                continue;
            }
            self.subs
                .insert(s.brsymbol.clone(), (s.symbol.clone(), s.exchange.clone()));
            tickers.push(s.brsymbol.clone());
        }
        if tickers.is_empty() {
            return Vec::new();
        }
        let mut v = vec![Self::sub_frame(&tickers, 1)];
        if !self.channel_resumed {
            v.push(Self::switch_frame());
            self.channel_resumed = true;
        }
        v
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let tickers: Vec<String> = subs
            .iter()
            .filter(|s| self.subs.remove(&s.brsymbol).is_some())
            .map(|s| s.brsymbol.clone())
            .collect();
        for t in &tickers {
            self.books.remove(t);
        }
        if tickers.is_empty() {
            Vec::new()
        } else {
            vec![Self::sub_frame(&tickers, -1)]
        }
    }

    fn mode_change_frames(
        &mut self,
        _old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        // Every TBT subscription is depth; only a new ticker needs a frame.
        if self.subs.contains_key(&new.brsymbol) {
            Vec::new()
        } else {
            self.subscribe_frames(std::slice::from_ref(new))
        }
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) if t == "pong" => vec![FeedEvent::Heartbeat],
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<Value>(t) {
                    if v.get("error").and_then(Value::as_bool) == Some(true) {
                        let detail = v.get("msg").and_then(Value::as_str).unwrap_or("");
                        tracing::warn!("Fyers TBT subscription error: {}", detail);
                    }
                }
                Vec::new()
            }
            Message::Binary(b) => {
                let Some(m) = decode_tbt(b) else {
                    tracing::debug!("Unreadable Fyers TBT frame ({} bytes)", b.len());
                    return Vec::new();
                };
                if m.error {
                    tracing::warn!("Fyers TBT error: {}", m.msg);
                    return Vec::new();
                }
                m.feeds
                    .iter()
                    .filter_map(|u| self.apply(u, m.snapshot))
                    .map(FeedEvent::Depth)
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((Duration::from_secs(10), Message::Text("ping".into())))
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[50]
    }
}
