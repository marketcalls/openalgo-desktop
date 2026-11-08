//! Kotak Neo market feed (SFeed `native_batch`; web
//! `streaming/sfeed_protocol.py`, `sfeed_websocket.py`, `kotak_adapter.py`).
//! Which host a data centre streams from is `feed_config`; order updates are
//! `order_feed`.
//!
//! SFeed control plane is JSON text:
//! * after connect: `{"user": <UCC>, "auth": <trading sid>, "format":
//!   "native_batch", "source": "NEOTRADEAPI", ...}`; the answer carries
//!   `message_code` 1117 or 1119 and per-exchange price `divider`s;
//!   `"format": "native_fallback"` means the server refused the binary
//!   format.
//! * `{"event": "subscribeScrips"|"subscribeDepth"|"subscribeIndices",
//!   "inputtoken": "nse_cm|11536,nse_cm|1594", "ack_symbol": true}` and the
//!   `unsubscribe*` twins; acks (1109) map `seg|token` to trading symbols.
//! * keepalive: a WebSocket ping every 30 s, answered within 10 s.
//!
//! The binary packets are decoded in `decode` (re-exported here).

mod decode;

pub use decode::*;

use super::data::{index_candidates, kotak_segment};
use super::feed_config::{lookup_feed_url, FeedLookup};
use crate::brokers::common::streaming::{
    now_ms, ping, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, PrepareError, WsRequest, WEB_PING_INTERVAL, WEB_PING_TIMEOUT,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const MSG_AUTH_RESPONSE: [u64; 2] = [1117, 1119];
pub const MSG_SUBSCRIBE_ACK: u64 = 1109;
/// Subscribed tokens per connection (web `MAX_SUBSCRIPTIONS`).
pub const MAX_SUBSCRIPTIONS: usize = 3000;

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
    /// The key the subscription was sent under (`seg|token` or
    /// `seg|<index name>`).
    input: String,
    /// Aliases an inbound packet may carry (token, every index name).
    aliases: Vec<(String, String)>,
}

/// Last known values of a subscribed instrument; zero means unchanged.
#[derive(Debug, Clone, Default)]
struct State {
    ltp: f64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: i64,
    average_price: f64,
    last_quantity: i64,
    total_buy: i64,
    total_sell: i64,
    oi: i64,
    ltt: i64,
    buy: Vec<DepthLevel>,
    sell: Vec<DepthLevel>,
    /// A depth packet (level 8 or 16) has filled the book.
    book: bool,
}

fn keep(old: f64, new: f64) -> f64 {
    if new != 0.0 {
        new
    } else {
        old
    }
}

fn keep_i(old: i64, new: i64) -> i64 {
    if new != 0 {
        new
    } else {
        old
    }
}

/// Merge new depth rows into the old per level, zero fields unchanged
/// (web adapter), padded to five.
fn merge_levels(old: &[DepthLevel], new: &[SfeedRow]) -> Vec<DepthLevel> {
    (0..5)
        .map(|i| {
            let o = old.get(i).copied().unwrap_or_default();
            match new.get(i) {
                Some(n) => DepthLevel {
                    price: keep(o.price, n.price),
                    quantity: keep_i(o.quantity, n.quantity),
                    orders: keep_i(o.orders, n.orders),
                },
                None => o,
            }
        })
        .collect()
}

fn epoch_ms(t: i64) -> i64 {
    if t <= 0 {
        0
    } else if t < 100_000_000_000 {
        t * 1000
    } else {
        t
    }
}

/// The Kotak SFeed market-data feed.
pub struct KotakFeed {
    url: String,
    lookup: Option<FeedLookup>,
    user: String,
    sid: String,
    dividers: HashMap<i8, f64>,
    /// `seg|token` -> trading symbol from subscribe acks (subscribed keys only).
    trading_symbols: HashMap<String, String>,
    subs: HashMap<String, SubInfo>,
    /// Inbound alias -> subscription key.
    aliases: HashMap<(String, String), String>,
    state: HashMap<String, State>,
}

impl KotakFeed {
    pub fn new(url: &str, sid: &str, ucc: String, _symbols: SymbolResolver) -> Self {
        Self {
            url: url.to_string(),
            lookup: None,
            user: if ucc.is_empty() { "neome".into() } else { ucc },
            sid: sid.to_string(),
            dividers: HashMap::new(),
            trading_symbols: HashMap::new(),
            subs: HashMap::new(),
            aliases: HashMap::new(),
            state: HashMap::new(),
        }
    }

    /// Look the data centre's feed host up in `prepare` (once) and remember
    /// it in the broker's `cache`.
    pub fn with_lookup(
        mut self,
        http: reqwest::Client,
        config_url: String,
        data_center: String,
        cache: std::sync::Arc<parking_lot::Mutex<Option<(String, String)>>>,
    ) -> Self {
        self.lookup = Some(FeedLookup {
            http,
            config_url,
            data_center,
            cache,
        });
        self
    }

    /// The address the next connect uses.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The `native_batch` authentication frame (web `_build_auth_frame`).
    pub fn auth_frame(&self) -> Value {
        json!({
            "user": self.user,
            "auth": if self.sid.is_empty() { "1" } else { self.sid.as_str() },
            "format": "native_batch",
            "source": "NEOTRADEAPI",
            "platform": "Web",
            "version": "1.2.3",
            "sdk_version": 2,
            "sdk_date": "2026-08-07T09:41:17.667Z",
            "conn_req_time": now_ms(),
            "sessionValidation": false,
        })
    }

    fn sub_key(s: &FeedSubscription) -> String {
        format!("{}:{}", s.exchange, s.symbol)
    }

    /// Register (or drop) a subscription and its aliases; returns the
    /// `inputtoken` entry it is sent as.
    fn register(&mut self, s: &FeedSubscription, add: bool) -> Option<(bool, String)> {
        let seg = kotak_segment(&s.exchange)?;
        let key = Self::sub_key(s);
        let index = s.exchange.to_ascii_uppercase().contains("INDEX");
        let names = if index {
            index_candidates(&s.symbol)
        } else {
            Vec::new()
        };
        let input = if index {
            format!("{}|{}", seg, names.first().cloned().unwrap_or_default())
        } else {
            if s.token.trim().is_empty() {
                return None;
            }
            format!("{}|{}", seg, s.token.trim())
        };
        if add {
            let mut aliases: Vec<(String, String)> = Vec::new();
            if !s.token.trim().is_empty() {
                aliases.push((seg.to_string(), s.token.trim().to_string()));
            }
            for n in &names {
                aliases.push((seg.to_string(), n.clone()));
                aliases.push((seg.to_string(), n.to_ascii_uppercase()));
            }
            if index {
                aliases.push((seg.to_string(), s.symbol.clone()));
            }
            for a in &aliases {
                self.aliases.insert(a.clone(), key.clone());
            }
            self.subs.insert(
                key,
                SubInfo {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    mode: s.mode,
                    input: input.clone(),
                    aliases,
                },
            );
        } else if let Some(old) = self.subs.remove(&key) {
            for a in &old.aliases {
                if self.aliases.get(a) == Some(&key) {
                    self.aliases.remove(a);
                }
            }
            self.state.remove(&key);
            self.trading_symbols.remove(&old.input);
        }
        Some((index, input))
    }

    fn event_frame(event: &str, inputs: &[String], ack: bool) -> Message {
        let mut v = json!({"event": event, "inputtoken": inputs.join(",")});
        if ack {
            v["ack_symbol"] = json!(true);
        }
        Message::Text(v.to_string())
    }

    fn frames(&mut self, subs: &[FeedSubscription], add: bool) -> Vec<Message> {
        let mut scrips = Vec::new();
        let mut depth = Vec::new();
        let mut indices = Vec::new();
        for s in subs {
            if add
                && !self.subs.contains_key(&Self::sub_key(s))
                && self.subs.len() >= MAX_SUBSCRIPTIONS
            {
                tracing::warn!(
                    "Kotak feed is at its {} instrument limit; {} not subscribed",
                    MAX_SUBSCRIPTIONS,
                    s.symbol
                );
                continue;
            }
            let Some((index, input)) = self.register(s, add) else {
                tracing::warn!("No Kotak feed key for {}:{}", s.exchange, s.symbol);
                continue;
            };
            if index {
                indices.push(input);
            } else {
                // Depth needs the quote stream too: Kotak sends depth and
                // LTP separately.
                if s.mode == FeedMode::Depth {
                    depth.push(input.clone());
                }
                scrips.push(input);
            }
        }
        let (ev_s, ev_d, ev_i) = if add {
            ("subscribeScrips", "subscribeDepth", "subscribeIndices")
        } else {
            (
                "unsubscribeScrips",
                "unsubscribeDepth",
                "unsubscribeIndices",
            )
        };
        let mut out = Vec::new();
        for (ev, list) in [(ev_s, scrips), (ev_d, depth), (ev_i, indices)] {
            if !list.is_empty() {
                out.push(Self::event_frame(ev, &list, add));
            }
        }
        out
    }

    fn lookup(&self, exchange: &str, token: &str, name: &str) -> Option<String> {
        self.aliases
            .get(&(exchange.to_string(), token.to_string()))
            .or_else(|| {
                (!name.is_empty())
                    .then(|| self.aliases.get(&(exchange.to_string(), name.to_string())))
                    .flatten()
            })
            .cloned()
    }

    fn tick_for(&self, key: &str, st: &State, now: i64) -> Option<NormalizedTick> {
        let sub = self.subs.get(key)?;
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: st.ltp,
            timestamp_ms: now,
            last_trade_time_ms: epoch_ms(st.ltt),
            ..Default::default()
        };
        if sub.mode != FeedMode::Ltp {
            t.open = st.open;
            t.high = st.high;
            t.low = st.low;
            t.close = st.close;
            t.volume = st.volume;
            t.average_price = st.average_price;
            t.last_quantity = st.last_quantity;
            t.total_buy_quantity = st.total_buy;
            t.total_sell_quantity = st.total_sell;
            t.oi = st.oi;
        }
        t.derive_change();
        Some(t)
    }

    /// A scrip packet: its fields folded into the instrument's state (a
    /// zero keeps the last value), a tick, and the depth in depth mode.
    fn on_scrip(&mut self, pkt: Sfeed, now: i64, out: &mut Vec<FeedEvent>) {
        let Sfeed::Scrip {
            exchange,
            token,
            level,
            ltp,
            open,
            high,
            low,
            close,
            average_price,
            last_trade_time,
            last_trade_qty,
            total_buy,
            total_sell,
            volume,
            oi,
            buy,
            sell,
        } = pkt
        else {
            return;
        };
        let Some(key) = self.lookup(&exchange, &token, "") else {
            return;
        };
        let st = self.state.entry(key.clone()).or_default();
        st.ltp = keep(st.ltp, ltp);
        st.open = keep(st.open, open);
        st.high = keep(st.high, high);
        st.low = keep(st.low, low);
        st.close = keep(st.close, close);
        st.average_price = keep(st.average_price, average_price);
        st.volume = keep_i(st.volume, volume);
        st.last_quantity = keep_i(st.last_quantity, last_trade_qty);
        st.total_buy = keep_i(st.total_buy, total_buy);
        st.total_sell = keep_i(st.total_sell, total_sell);
        st.oi = keep_i(st.oi, oi);
        st.ltt = keep_i(st.ltt, last_trade_time);
        if matches!(level, 4 | 8 | 16) {
            st.buy = merge_levels(&st.buy, &buy);
            st.sell = merge_levels(&st.sell, &sell);
            st.book |= level != 4;
        }
        let st = st.clone();
        let mode = self.subs.get(&key).map(|s| s.mode);
        if mode == Some(FeedMode::Ltp) && st.ltp <= 0.0 {
            return;
        }
        self.publish(&key, &st, now, out);
    }

    /// A packet's tick, and for a Depth subscription whose book has
    /// arrived, that book with the packet's price. Kotak sends the price
    /// and the book in separate packets; the web republishes the depth with
    /// its cached book on every packet (`kotak_adapter._on_data_received`,
    /// mode 3), so the option chain, the order dialog and every Depth
    /// holder see the price move between book updates.
    fn publish(&self, key: &str, st: &State, now: i64, out: &mut Vec<FeedEvent>) {
        let Some(sub) = self.subs.get(key) else {
            return;
        };
        if let Some(t) = self.tick_for(key, st, now) {
            out.push(FeedEvent::Tick(t));
        }
        if sub.mode == FeedMode::Depth && st.book {
            out.push(FeedEvent::Depth(NormalizedDepth {
                symbol: sub.symbol.clone(),
                exchange: sub.exchange.clone(),
                ltp: st.ltp,
                buy: st.buy.clone(),
                sell: st.sell.clone(),
                total_buy_quantity: st.total_buy,
                total_sell_quantity: st.total_sell,
                timestamp_ms: now,
            }));
        }
    }

    fn on_packet(&mut self, pkt: Sfeed, out: &mut Vec<FeedEvent>) {
        let now = now_ms();
        match pkt {
            pkt @ Sfeed::Scrip { .. } => self.on_scrip(pkt, now, out),
            Sfeed::Lite {
                exchange,
                token,
                ltp,
                close,
                last_trade_time,
                last_trade_qty,
            } => {
                let Some(key) = self.lookup(&exchange, &token, "") else {
                    return;
                };
                let st = self.state.entry(key.clone()).or_default();
                st.ltp = keep(st.ltp, ltp);
                st.close = keep(st.close, close);
                st.ltt = keep_i(st.ltt, last_trade_time);
                st.last_quantity = keep_i(st.last_quantity, last_trade_qty);
                let st = st.clone();
                if st.ltp <= 0.0 {
                    return;
                }
                self.publish(&key, &st, now, out);
            }
            Sfeed::Index {
                exchange,
                token,
                name,
                value,
                open,
                high,
                low,
                close,
                last_trade_time,
            } => {
                // An index answers with a token of Kotak's choosing; its name
                // is the identity it was subscribed under.
                let Some(key) = self
                    .lookup(&exchange, &token, &name)
                    .or_else(|| self.lookup(&exchange, "", &name.to_ascii_uppercase()))
                else {
                    return;
                };
                let st = self.state.entry(key.clone()).or_default();
                st.ltp = keep(st.ltp, value);
                st.open = keep(st.open, open);
                st.high = keep(st.high, high);
                st.low = keep(st.low, low);
                st.close = keep(st.close, close);
                st.ltt = keep_i(st.ltt, last_trade_time);
                let st = st.clone();
                if st.ltp <= 0.0 {
                    return;
                }
                if let Some(t) = self.tick_for(&key, &st, now) {
                    out.push(FeedEvent::Tick(t));
                }
            }
            Sfeed::MarketStatus { exchange, code } => {
                tracing::info!("Kotak market status: {} code {}", exchange, code);
            }
            Sfeed::Cas { .. } => {}
        }
    }

    /// Decode one binary frame.
    pub fn parse_binary(&mut self, frame: &[u8]) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        let packets: Vec<Sfeed> = split_batch(frame)
            .into_iter()
            .filter_map(|p| decode_packet(p, &self.dividers))
            .collect();
        for p in packets {
            self.on_packet(p, &mut out);
        }
        out
    }

    /// One JSON control frame.
    pub fn parse_text(&mut self, text: &str) -> Vec<FeedEvent> {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            tracing::debug!("Kotak feed non-JSON text frame");
            return Vec::new();
        };
        let code = v.get("message_code").and_then(Value::as_u64);
        match code {
            Some(c) if MSG_AUTH_RESPONSE.contains(&c) => {
                if v.get("format").and_then(Value::as_str) == Some("native_fallback") {
                    tracing::error!("Kotak SFeed downgraded the connection to native_fallback");
                    return vec![FeedEvent::AuthFailed(
                        "Kotak's live market data refused this connection. Log in to Kotak again; if it keeps happening, contact Kotak support."
                            .into(),
                    )];
                }
                self.dividers = parse_dividers(&v);
                vec![FeedEvent::AuthOk]
            }
            Some(MSG_SUBSCRIBE_ACK) => {
                let map = v
                    .get("trading_symbols")
                    .or_else(|| v.get("tradingSymbols"))
                    .and_then(Value::as_object);
                if let Some(m) = map {
                    for (k, sym) in m {
                        // Only for keys we subscribed: bounded by the subs.
                        if self.subs.values().any(|s| &s.input == k) {
                            if let Some(sym) = sym.as_str() {
                                self.trading_symbols.insert(k.clone(), sym.to_string());
                            }
                        }
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Instruments currently registered (tests and diagnostics).
    pub fn subscription_count(&self) -> usize {
        self.subs.len()
    }

    /// Cached per-instrument state entries (bounded by subscriptions).
    pub fn state_len(&self) -> usize {
        self.state.len()
    }
}

#[async_trait::async_trait]
impl BrokerFeed for KotakFeed {
    fn broker(&self) -> &'static str {
        "kotak"
    }

    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        if let Some(l) = self.lookup.take() {
            let url = lookup_feed_url(&l.http, &l.config_url, &l.data_center).await;
            *l.cache.lock() = Some((l.data_center.clone(), url.clone()));
            self.url = url;
        }
        Ok(())
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Kotak feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // A new connection authenticates again; old dividers and partial
        // state do not carry over.
        self.dividers.clear();
        self.state.clear();
        vec![Message::Text(self.auth_frame().to_string())]
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    /// The web pings the feed every 30 s and drops a socket that has not
    /// answered 10 s later (`sfeed_websocket.py`: `run_forever(
    /// ping_interval=30, ping_timeout=10)`).
    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((WEB_PING_INTERVAL, ping()))
    }

    fn heartbeat_reply_timeout(&self) -> Option<Duration> {
        Some(WEB_PING_TIMEOUT)
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
        let index = new.exchange.to_ascii_uppercase().contains("INDEX");
        let was_depth = old.mode == FeedMode::Depth;
        let is_depth = new.mode == FeedMode::Depth;
        self.register(new, true);
        if index || was_depth == is_depth {
            return Vec::new();
        }
        let Some(input) = self.subs.get(&Self::sub_key(new)).map(|s| s.input.clone()) else {
            return Vec::new();
        };
        if is_depth {
            vec![Self::event_frame("subscribeDepth", &[input], true)]
        } else {
            // The book goes with the depth stream; the price and the quote
            // fields stay, so the next packet does not read them as zero.
            if let Some(st) = self.state.get_mut(&Self::sub_key(new)) {
                st.buy.clear();
                st.sell.clear();
                st.book = false;
            }
            vec![Self::event_frame("unsubscribeDepth", &[input], false)]
        }
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            Message::Text(t) => self.parse_text(t),
            _ => Vec::new(),
        }
    }
}
