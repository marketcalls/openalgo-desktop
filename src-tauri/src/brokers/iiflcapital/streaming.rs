//! IIFL Capital feeds over the MQTT bridge (web
//! `streaming/iiflcapital_websocket.py`, `iiflcapital_adapter.py`,
//! `iiflcapital_order_adapter.py`).
//!
//! Credentials (bridgePy format): username = the session JWT's
//! `preferred_username`, password = `OPENID~~<session>~`, client id =
//! `openalgo` + `%d%m%y%H%M%S%f` + 4 random bytes hex (a fresh one per
//! connection, so a reconnect never collides with the old session).
//!
//! Market data topics (`segment` is the stored brexchange lowercased):
//! `prod/marketfeed/mw/v1/<segment>/<token>` (instruments),
//! `prod/marketfeed/index/v1/<segment>/<token>` (indices),
//! `prod/marketfeed/oi/v1/<segment>/<token>` (open interest, derivatives
//! in Quote/Depth mode). One packet shape carries everything, so each
//! instrument is subscribed once at its highest mode and the tick is sliced
//! per mode.
//!
//! Order updates: `prod/updates/order/v1/<clientId>` and
//! `prod/updates/trade/v1/<clientId>`, JSON packets.

use super::mapping;
use super::mqtt_relay::{self, MqttEndpoint, MqttRelay, MqttUpstream, Prepare, Prepared};
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::brokers::upstox::relay;
use crate::error::{AppError, Result};
use crate::security::Secret;
use async_trait::async_trait;
use base64::Engine;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const TOPIC_MARKET_FEED: &str = "prod/marketfeed/mw/v1/";
pub const TOPIC_INDEX_FEED: &str = "prod/marketfeed/index/v1/";
pub const TOPIC_OPEN_INTEREST: &str = "prod/marketfeed/oi/v1/";
pub const TOPIC_ORDER_UPDATE: &str = "prod/updates/order/v1/";
pub const TOPIC_TRADE_UPDATE: &str = "prod/updates/trade/v1/";

/// web `MAX_INSTRUMENTS_PER_CONNECTION`.
pub const MAX_INSTRUMENTS: usize = 5800;

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// `preferred_username` claim of the session JWT (bridgePy
/// `__get_user_name`).
pub fn jwt_username(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("preferred_username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `<prefix>` + `%d%m%y%H%M%S%f` + 8 hex chars.
pub fn client_id(prefix: &str) -> String {
    format!(
        "{}{}{}",
        prefix,
        chrono::Local::now().format("%d%m%y%H%M%S%6f"),
        hex::encode(rand::random::<[u8; 4]>())
    )
}

fn credentials(prefix: &str, session: &str, topics: Vec<String>) -> Prepare {
    match jwt_username(session) {
        Some(username) => Prepare::Ready(Prepared {
            client_id: client_id(prefix),
            username,
            password: format!("OPENID~~{}~", session),
            topics,
        }),
        None => Prepare::AuthFailed(
            "The IIFL Capital session cannot be used for live data. Log in to IIFL Capital again."
                .into(),
        ),
    }
}

// ---------------------------------------------------------------------------
// MWBOCombined (C# struct, Pack=2, little-endian)
// ---------------------------------------------------------------------------

/// Byte offsets of `MWBOCombined` (web `_MWBOCombined`, `_pack_ = 2`).
pub mod offsets {
    pub const LTP: usize = 0; // int32
    pub const LAST_TRADED_QTY: usize = 4; // uint32
    pub const TRADED_VOLUME: usize = 8; // uint32
    pub const HIGH: usize = 12; // int32
    pub const LOW: usize = 16; // int32
    pub const OPEN: usize = 20; // int32
    pub const CLOSE: usize = 24; // int32
    pub const AVG_TRADED_PRICE: usize = 28; // int32
    pub const RESERVED: usize = 32; // uint16
    pub const BEST_BID_QTY: usize = 34; // uint32 (2-byte packing: no pad)
    pub const BEST_BID_PRICE: usize = 38; // int32
    pub const BEST_ASK_QTY: usize = 42; // uint32
    pub const BEST_ASK_PRICE: usize = 46; // int32
    pub const TOTAL_BID_QTY: usize = 50; // uint32
    pub const TOTAL_ASK_QTY: usize = 54; // uint32
    pub const PRICE_DIVISOR: usize = 58; // int32
    pub const LAST_TRADED_TIME: usize = 62; // int32
    /// Ten `Depth` entries of 12 bytes: quantity uint32 +0, price int32 +4,
    /// orders int16 +8, transactionType int16 +10. Bids are the first five,
    /// asks the last five (positional; transactionType is unreliable).
    pub const DEPTH: usize = 66;
    pub const DEPTH_ENTRY: usize = 12;
    /// `ctypes.sizeof(_MWBOCombined)`; the bridge publishes 188 bytes.
    pub const SIZE: usize = 186;
}

fn i32_at(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn i16_at(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}

/// A decoded market packet, prices in rupees.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarketPacket {
    pub ltp: f64,
    pub last_traded_quantity: i64,
    pub volume: i64,
    pub high: f64,
    pub low: f64,
    pub open: f64,
    pub close: f64,
    pub average_price: f64,
    pub best_bid_quantity: i64,
    pub best_bid_price: f64,
    pub best_ask_quantity: i64,
    pub best_ask_price: f64,
    pub total_buy_quantity: i64,
    pub total_sell_quantity: i64,
    /// Last trade time as sent (epoch seconds).
    pub ltt: i64,
    pub buy: Vec<DepthLevel>,
    pub sell: Vec<DepthLevel>,
}

/// Decode an `MWBOCombined` payload (web `_decode_market_feed`); `None`
/// when shorter than 186 bytes. Prices are divided by `priceDivisor`
/// (100 when the packet says 0).
pub fn decode_market(p: &[u8]) -> Option<MarketPacket> {
    use offsets::*;
    if p.len() < SIZE {
        return None;
    }
    let div = match i32_at(p, PRICE_DIVISOR) {
        0 => 100.0,
        d => f64::from(d),
    };
    let price = |o: usize| f64::from(i32_at(p, o)) / div;
    let level = |i: usize| {
        let o = DEPTH + i * DEPTH_ENTRY;
        DepthLevel {
            quantity: i64::from(u32_at(p, o)),
            price: f64::from(i32_at(p, o + 4)) / div,
            orders: i64::from(i16_at(p, o + 8)),
        }
    };
    Some(MarketPacket {
        ltp: price(LTP),
        last_traded_quantity: i64::from(u32_at(p, LAST_TRADED_QTY)),
        volume: i64::from(u32_at(p, TRADED_VOLUME)),
        high: price(HIGH),
        low: price(LOW),
        open: price(OPEN),
        close: price(CLOSE),
        average_price: price(AVG_TRADED_PRICE),
        best_bid_quantity: i64::from(u32_at(p, BEST_BID_QTY)),
        best_bid_price: price(BEST_BID_PRICE),
        best_ask_quantity: i64::from(u32_at(p, BEST_ASK_QTY)),
        best_ask_price: price(BEST_ASK_PRICE),
        total_buy_quantity: i64::from(u32_at(p, TOTAL_BID_QTY)),
        total_sell_quantity: i64::from(u32_at(p, TOTAL_ASK_QTY)),
        ltt: i64::from(i32_at(p, LAST_TRADED_TIME)),
        buy: (0..5).map(level).collect(),
        sell: (5..10).map(level).collect(),
    })
}

/// The 16-byte OI packet: four little-endian int32 (oi, day high, day low,
/// previous) (web `_decode_open_interest`). Returns the OI.
pub fn decode_oi(p: &[u8]) -> Option<i64> {
    if p.len() < 16 {
        return None;
    }
    Some(i64::from(i32_at(p, 0)))
}

// ---------------------------------------------------------------------------
// Market-data feed
// ---------------------------------------------------------------------------

fn is_index(exchange: &str) -> bool {
    matches!(
        exchange,
        "NSE_INDEX" | "BSE_INDEX" | "MCX_INDEX" | "GLOBAL_INDEX"
    )
}

/// `nseeq/2885`.
pub fn topic_key(sub: &FeedSubscription) -> String {
    format!(
        "{}/{}",
        sub.brexchange.trim().to_ascii_lowercase(),
        sub.token.trim()
    )
}

/// Topics for one subscription at its effective mode.
pub fn topics_for(sub: &FeedSubscription) -> Vec<String> {
    let key = topic_key(sub);
    if is_index(&sub.exchange) {
        return vec![format!("{}{}", TOPIC_INDEX_FEED, key)];
    }
    let mut v = vec![format!("{}{}", TOPIC_MARKET_FEED, key)];
    if sub.mode != FeedMode::Ltp && super::data::supports_oi(&sub.exchange) {
        v.push(format!("{}{}", TOPIC_OPEN_INTEREST, key));
    }
    v
}

struct MarketUpstream {
    endpoint: MqttEndpoint,
    session: Secret,
}

#[async_trait]
impl MqttUpstream for MarketUpstream {
    fn broker(&self) -> &'static str {
        "iiflcapital"
    }

    fn endpoint(&self) -> &MqttEndpoint {
        &self.endpoint
    }

    async fn prepare(&self) -> Prepare {
        credentials("openalgo", self.session.expose(), Vec::new())
    }
}

/// The market-data `BrokerFeed`.
pub struct IiflFeed {
    upstream: Arc<MarketUpstream>,
    relay: Mutex<Option<MqttRelay>>,
    /// Topic key -> subscription (bounded by the manager's subscriptions).
    subs: HashMap<String, FeedSubscription>,
    /// Latest OI per topic key (removed with the subscription).
    oi: HashMap<String, i64>,
}

impl IiflFeed {
    pub fn new(endpoint: MqttEndpoint, session: &str, _symbols: SymbolResolver) -> Result<Self> {
        if jwt_username(session).is_none() {
            return Err(AppError::Auth(
                "The IIFL Capital session cannot be used for live data. Log in to IIFL Capital again."
                    .into(),
            ));
        }
        Ok(Self {
            upstream: Arc::new(MarketUpstream {
                endpoint,
                session: Secret::new(session),
            }),
            relay: Mutex::new(None),
            subs: HashMap::new(),
            oi: HashMap::new(),
        })
    }

    fn on_publish(&mut self, topic: &str, payload: &[u8]) -> Vec<FeedEvent> {
        if let Some(key) = topic.strip_prefix(TOPIC_OPEN_INTEREST) {
            if let (Some(v), true) = (decode_oi(payload), self.subs.contains_key(key)) {
                self.oi.insert(key.to_string(), v);
            }
            return Vec::new();
        }
        let key = topic
            .strip_prefix(TOPIC_MARKET_FEED)
            .or_else(|| topic.strip_prefix(TOPIC_INDEX_FEED));
        let Some(key) = key else {
            return Vec::new();
        };
        let (Some(sub), Some(p)) = (self.subs.get(key), decode_market(payload)) else {
            return Vec::new();
        };
        ticks(sub, &p, self.oi.get(key).copied())
    }
}

/// Slice one packet into the subscription's mode (web `_handle_ticks`).
pub fn ticks(sub: &FeedSubscription, p: &MarketPacket, oi: Option<i64>) -> Vec<FeedEvent> {
    let now = now_ms();
    let mut t = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode: sub.mode.code(),
        ltp: p.ltp,
        last_trade_time_ms: p.ltt.saturating_mul(1000),
        timestamp_ms: now,
        ..Default::default()
    };
    if sub.mode != FeedMode::Ltp {
        t.open = p.open;
        t.high = p.high;
        t.low = p.low;
        t.close = p.close;
        t.volume = p.volume;
        t.last_quantity = p.last_traded_quantity;
        t.average_price = p.average_price;
        t.total_buy_quantity = p.total_buy_quantity;
        t.total_sell_quantity = p.total_sell_quantity;
        t.oi = oi.unwrap_or(0);
        t.derive_change();
    }
    let mut out = vec![FeedEvent::Tick(t)];
    if sub.mode == FeedMode::Depth {
        out.push(FeedEvent::Depth(NormalizedDepth {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            ltp: p.ltp,
            buy: p.buy.clone(),
            sell: p.sell.clone(),
            total_buy_quantity: p.total_buy_quantity,
            total_sell_quantity: p.total_sell_quantity,
            timestamp_ms: now,
        }));
    }
    out
}

fn relay_events(msg: &Message) -> Option<Vec<FeedEvent>> {
    match msg {
        Message::Text(t) => Some(match relay::control(t) {
            Some(Ok(())) => vec![FeedEvent::AuthOk],
            Some(Err(m)) => vec![FeedEvent::AuthFailed(m)],
            None => Vec::new(),
        }),
        Message::Ping(_) | Message::Pong(_) => Some(vec![FeedEvent::Heartbeat]),
        _ => None,
    }
}

impl BrokerFeed for IiflFeed {
    fn broker(&self) -> &'static str {
        "iiflcapital"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let up = self.upstream.clone();
        let url = mqtt_relay::ensure_started(&self.relay, move || up as Arc<dyn MqttUpstream>)?;
        url.as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("IIFL Capital feed relay address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // Clean session: every topic is subscribed again after READY.
        self.subs.clear();
        self.oi.clear();
        Vec::new()
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut topics = Vec::new();
        for s in subs {
            let key = topic_key(s);
            if !self.subs.contains_key(&key) && self.subs.len() >= MAX_INSTRUMENTS {
                tracing::warn!(
                    "IIFL Capital allows {} live instruments per connection; {} was not subscribed",
                    MAX_INSTRUMENTS,
                    s.symbol
                );
                continue;
            }
            topics.extend(topics_for(s));
            self.subs.insert(key, s.clone());
        }
        if topics.is_empty() {
            Vec::new()
        } else {
            vec![mqtt_relay::sub_frame(&topics)]
        }
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut topics = Vec::new();
        for s in subs {
            let key = topic_key(s);
            if let Some(old) = self.subs.remove(&key) {
                topics.extend(topics_for(&old));
            }
            self.oi.remove(&key);
        }
        if topics.is_empty() {
            Vec::new()
        } else {
            vec![mqtt_relay::unsub_frame(&topics)]
        }
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        if let Some(ev) = relay_events(msg) {
            return ev;
        }
        match msg {
            Message::Binary(b) => match mqtt_relay::decode_publish(b) {
                Some((topic, payload)) => {
                    let topic = topic.to_string();
                    self.on_publish(&topic, payload)
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}

// ---------------------------------------------------------------------------
// Order-update feed
// ---------------------------------------------------------------------------

struct OrderUpstream {
    endpoint: MqttEndpoint,
    session: Secret,
    base_url: String,
    client_id: Mutex<Option<String>>,
    http: reqwest::Client,
}

impl OrderUpstream {
    /// The IIFL client id: known from the session, or `GET /profile`
    /// `result.clientId` (web `_fetch_client_id`).
    async fn client_id(&self) -> Option<String> {
        if let Some(c) = self.client_id.lock().clone() {
            return Some(c);
        }
        let resp = self
            .http
            .get(format!("{}/profile", self.base_url))
            .header("Authorization", format!("Bearer {}", self.session.expose()))
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            tracing::warn!(
                status = resp.status().as_u16(),
                "IIFL Capital profile lookup for order updates failed"
            );
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        let id = mapping::text(v.get("result").and_then(|r| r.get("clientId")));
        if id.is_empty() {
            return None;
        }
        *self.client_id.lock() = Some(id.clone());
        Some(id)
    }
}

#[async_trait]
impl MqttUpstream for OrderUpstream {
    fn broker(&self) -> &'static str {
        "iiflcapital"
    }

    fn endpoint(&self) -> &MqttEndpoint {
        &self.endpoint
    }

    async fn prepare(&self) -> Prepare {
        let Some(id) = self.client_id().await else {
            return Prepare::Unavailable;
        };
        // Topic items must match `^[0-9a-z/]+$` (bridgePy); client ids are
        // numeric, lowercase anything else.
        let id = id.to_ascii_lowercase();
        credentials(
            "openalgo-orderupdate",
            self.session.expose(),
            order_topics(&id),
        )
    }
}

pub fn order_topics(client_id: &str) -> Vec<String> {
    vec![
        format!("{}{}", TOPIC_ORDER_UPDATE, client_id),
        format!("{}{}", TOPIC_TRADE_UPDATE, client_id),
    ]
}

/// Exchanges probed for an order packet's instrument (order packets carry
/// no exchange; web `_CANDIDATE_EXCHANGES`).
const CANDIDATE_EXCHANGES: &[&str] = &["NSE", "BSE", "NFO", "BFO", "MCX", "CDS", "BCD"];

/// Normalise an order-update packet (web `_normalize_order`).
pub fn order_packet(d: &Value, symbols: &SymbolResolver) -> OrderUpdate {
    let status = mapping::map_status(&mapping::text(d.get("orderStatus"))).to_string();
    let token = mapping::text(d.get("instrumentId"));
    let tsym = mapping::text(d.get("tradingSymbol"));
    let found = (!token.is_empty())
        .then(|| {
            CANDIDATE_EXCHANGES
                .iter()
                .find_map(|ex| symbols.by_token(ex, &token))
        })
        .flatten();
    let (symbol, exchange) = match found {
        Some(r) => (r.symbol, r.exchange),
        None => (tsym, String::new()),
    };
    OrderUpdate {
        orderid: mapping::text(mapping::first(d, &["brokerOrderId", "exchangeOrderId"])),
        symbol,
        exchange,
        action: mapping::action(&mapping::text(d.get("transactionType"))),
        quantity: mapping::int(d.get("quantity")),
        price: mapping::num(d.get("price")),
        trigger_price: mapping::num(d.get("slTriggerPrice")),
        pricetype: mapping::order_type_from_broker(&mapping::text(d.get("orderType"))).into(),
        product: mapping::product_from_broker(&mapping::text(d.get("product"))).into(),
        filled_quantity: mapping::int(d.get("filledQuantity")),
        pending_quantity: mapping::int(d.get("pendingQuantity")),
        average_price: mapping::num(d.get("averageTradedPrice")),
        rejection_reason: if status == "rejected" {
            mapping::text(d.get("rejectionReason"))
        } else {
            String::new()
        },
        order_status: status,
    }
}

/// Normalise a trade-update packet (web `_normalize_trade`): a fill is
/// reported as complete at the traded price.
pub fn trade_packet(d: &Value, symbols: &SymbolResolver) -> OrderUpdate {
    let token = mapping::text(d.get("instrumentId"));
    let tsym = mapping::text(d.get("tradingSymbol"));
    let exchange = mapping::from_segment(&mapping::text(d.get("exchange")));
    let symbol = if exchange.is_empty() {
        tsym
    } else {
        symbols
            .by_token(&exchange, &token)
            .map(|r| r.symbol)
            .unwrap_or_else(|| symbols.oa_symbol_or_raw(&tsym, &exchange))
    };
    let qty = mapping::int(d.get("filledQuantity"));
    let price = mapping::num(d.get("tradedPrice"));
    OrderUpdate {
        orderid: mapping::text(mapping::first(d, &["brokerOrderId", "exchangeOrderId"])),
        symbol,
        exchange,
        action: mapping::action(&mapping::text(d.get("transactionType"))),
        quantity: qty,
        price,
        trigger_price: 0.0,
        pricetype: mapping::order_type_from_broker(&mapping::text(d.get("orderType"))).into(),
        product: mapping::product_from_broker(&mapping::text(d.get("product"))).into(),
        order_status: "complete".into(),
        filled_quantity: qty,
        pending_quantity: 0,
        average_price: price,
        rejection_reason: String::new(),
    }
}

/// Order and trade updates as a `BrokerFeed` (no instrument subscriptions;
/// the relay subscribes both topics as soon as the bridge accepts).
pub struct IiflOrderFeed {
    upstream: Arc<OrderUpstream>,
    relay: Mutex<Option<MqttRelay>>,
    symbols: SymbolResolver,
}

impl IiflOrderFeed {
    pub fn new(
        endpoint: MqttEndpoint,
        session: &str,
        client_id: Option<String>,
        base_url: String,
        symbols: SymbolResolver,
    ) -> Result<Self> {
        if jwt_username(session).is_none() {
            return Err(AppError::Auth(
                "The IIFL Capital session cannot be used for order updates. Log in to IIFL Capital again."
                    .into(),
            ));
        }
        Ok(Self {
            upstream: Arc::new(OrderUpstream {
                endpoint,
                session: Secret::new(session),
                base_url,
                client_id: Mutex::new(client_id.filter(|c| !c.trim().is_empty())),
                http: crate::brokers::common::http::client(),
            }),
            relay: Mutex::new(None),
            symbols,
        })
    }

    /// Decode one relayed PUBLISH.
    pub fn on_publish(&self, topic: &str, payload: &[u8]) -> Vec<FeedEvent> {
        let Ok(d) = serde_json::from_slice::<Value>(payload) else {
            return Vec::new();
        };
        if !d.is_object() {
            return Vec::new();
        }
        let u = if topic.starts_with(TOPIC_ORDER_UPDATE) {
            order_packet(&d, &self.symbols)
        } else if topic.starts_with(TOPIC_TRADE_UPDATE) {
            trade_packet(&d, &self.symbols)
        } else {
            return Vec::new();
        };
        vec![FeedEvent::OrderUpdate(u)]
    }
}

impl BrokerFeed for IiflOrderFeed {
    fn broker(&self) -> &'static str {
        "iiflcapital"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let up = self.upstream.clone();
        let url = mqtt_relay::ensure_started(&self.relay, move || up as Arc<dyn MqttUpstream>)?;
        url.as_str().into_client_request().map_err(|_| {
            AppError::Internal("IIFL Capital order-update relay address is invalid".into())
        })
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        if let Some(ev) = relay_events(msg) {
            return ev;
        }
        match msg {
            Message::Binary(b) => match mqtt_relay::decode_publish(b) {
                Some((topic, payload)) => self.on_publish(topic, payload),
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }
}
