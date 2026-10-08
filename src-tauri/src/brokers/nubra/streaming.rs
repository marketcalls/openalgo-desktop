//! Nubra live feeds (web `api/nubrawebsocket.py`, `streaming/nubra_adapter.py`,
//! `streaming/nubra_order_adapter.py`).
//!
//! Market data: `wss://api.nubra.io/apibatch/ws` with `Authorization: Bearer
//! <session>` and `x-device-id`. Subscriptions are text frames that carry the
//! session token:
//!
//! * `batch_subscribe <tok> index {"instruments":[],"indexes":[names]} <EX>`
//! * `batch_subscribe <tok> index_bucket {..} 1d <EX>` (open of the day)
//! * `batch_subscribe <tok> orderbook {"instruments":[ref_ids],"indexes":[]}`
//!   followed by `batch_subscribe <tok> orderbook_depth 5` (starts the flow)
//! * `batch_unsubscribe ...` with the same shapes.
//!
//! The index channel is keyed by name: indices subscribe under the web's
//! `SUBSCRIPTION_MAP` name (else the broker symbol), instruments under their
//! OpenAlgo symbol; incoming `indexname` is upper-cased and mapped back
//! through `INDEX_NAME_MAP`. Non-index instruments also ride the order-book
//! channel in every mode: the web's adapter notes that channel is the only
//! one that reliably ticks for them (issue #1664) and fans LTP / quote out of
//! it, so subscribing it for LTP / quote too keeps those subscribers fed.
//!
//! Frames are binary Any-in-Any protobuf (`proto.rs`); a text frame
//! `Invalid Token` means the session is gone.
//!
//! Order updates: the socket named by `GET /userinfo` (`env_info.user_ws_url`),
//! same headers, then the text frame `subscribe <tok> notifications
//! notification`. Frames are Any-in-Any `NubraToClientIntentUpdate`, decoded
//! field by field (numbers from `nubra_order_adapter.py:normalize`).

use super::data::{feed_name, ws_exchange, SUBSCRIPTION_MAP};
use super::mapping;
use super::proto::{self, MarketFrame};
use super::DEVICE_ID;
use crate::brokers::common::relay::{self, Open, RelayHandle, Session, Step, Upstream};
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::Secret;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Socket keepalive (web `run_forever(ping_interval=20)`).
pub const PING_INTERVAL: Duration = Duration::from_secs(20);
/// Order-book levels requested (web `change_orderbook_depth(5)`).
pub const DEPTH_LEVELS: u8 = 5;

/// `{"instruments":[..],"indexes":[..]}` with the web's key order and no
/// spaces (`json.dumps(..., separators=(',', ':'))`).
pub fn batch_payload(ref_ids: &[i64], names: &[String]) -> String {
    format!(
        "{{\"instruments\":{},\"indexes\":{}}}",
        serde_json::to_string(ref_ids).unwrap_or_else(|_| "[]".into()),
        serde_json::to_string(names).unwrap_or_else(|_| "[]".into())
    )
}

fn is_index(exchange: &str) -> bool {
    exchange.ends_with("_INDEX")
}

/// Merged per-instrument state (prices in rupees).
#[derive(Debug, Clone, Default)]
struct State {
    ltp: f64,
    open: f64,
    high: f64,
    low: f64,
    prev_close: f64,
    volume: i64,
    ltq: i64,
    oi: i64,
    change_percent: f64,
    has_index: bool,
    bids: Vec<DepthLevel>,
    asks: Vec<DepthLevel>,
}

#[derive(Debug, Clone)]
struct Inst {
    sub: FeedSubscription,
    /// Name sent on the index channel.
    sub_name: String,
    /// Names incoming index frames are matched on.
    name_keys: Vec<String>,
    ws_ex: &'static str,
    ref_id: Option<i64>,
    state: State,
}

type Key = (String, String);

/// The market-data feed.
pub struct NubraFeed {
    url: String,
    token: Secret,
    insts: HashMap<Key, Inst>,
    by_name: HashMap<String, Key>,
    by_ref: HashMap<i64, Key>,
}

/// Index-channel name for a subscription (web `_subscribe_via_index_channel`).
pub fn subscription_name(s: &FeedSubscription) -> String {
    if is_index(&s.exchange) {
        SUBSCRIPTION_MAP
            .iter()
            .find(|(k, _)| *k == s.symbol)
            .map(|(_, v)| v.to_string())
            .unwrap_or_else(|| {
                if s.brsymbol.is_empty() {
                    s.symbol.clone()
                } else {
                    s.brsymbol.clone()
                }
            })
    } else {
        s.symbol.clone()
    }
}

fn levels(src: &[proto::OrderBookLevel]) -> Vec<DepthLevel> {
    let mut v: Vec<DepthLevel> = src
        .iter()
        .take(5)
        .map(|l| DepthLevel {
            price: l.price as f64 / 100.0,
            quantity: l.quantity,
            orders: l.orders,
        })
        .collect();
    v.resize(5, DepthLevel::default());
    v
}

impl NubraFeed {
    pub fn new(url: &str, token: &str, _symbols: SymbolResolver) -> Self {
        Self {
            url: url.to_string(),
            token: Secret::new(token),
            insts: HashMap::new(),
            by_name: HashMap::new(),
            by_ref: HashMap::new(),
        }
    }

    /// Instruments tracked (bounded by the subscriptions).
    pub fn tracked(&self) -> usize {
        self.insts.len()
    }

    fn frames(&self, verb: &str, subs: &[&Inst]) -> Vec<Message> {
        let tok = self.token.expose();
        let mut names: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
        let mut refs: Vec<i64> = Vec::new();
        for i in subs {
            names.entry(i.ws_ex).or_default().push(i.sub_name.clone());
            if let Some(r) = i.ref_id {
                refs.push(r);
            }
        }
        let mut out = Vec::new();
        for (ex, n) in &names {
            let p = batch_payload(&[], n);
            out.push(Message::Text(format!(
                "{} {} index {} {}",
                verb, tok, p, ex
            )));
            out.push(Message::Text(format!(
                "{} {} index_bucket {} 1d {}",
                verb, tok, p, ex
            )));
        }
        if !refs.is_empty() {
            out.push(Message::Text(format!(
                "{} {} orderbook {}",
                verb,
                tok,
                batch_payload(&refs, &[])
            )));
            if verb == "batch_subscribe" {
                out.push(Message::Text(format!(
                    "batch_subscribe {} orderbook_depth {}",
                    tok, DEPTH_LEVELS
                )));
            }
        }
        out
    }

    fn forget(&mut self, key: &Key) -> Option<Inst> {
        let inst = self.insts.remove(key)?;
        for n in &inst.name_keys {
            if self.by_name.get(n) == Some(key) {
                self.by_name.remove(n);
            }
        }
        if let Some(r) = inst.ref_id {
            if self.by_ref.get(&r) == Some(key) {
                self.by_ref.remove(&r);
            }
        }
        Some(inst)
    }

    fn tick(inst: &Inst) -> NormalizedTick {
        let st = &inst.state;
        let mut t = NormalizedTick {
            symbol: inst.sub.symbol.clone(),
            exchange: inst.sub.exchange.clone(),
            mode: inst.sub.mode.code(),
            ltp: st.ltp,
            timestamp_ms: now_ms(),
            ..Default::default()
        };
        if inst.sub.mode != FeedMode::Ltp {
            t.open = st.open;
            t.high = st.high;
            t.low = st.low;
            t.close = st.prev_close;
            t.volume = st.volume;
            t.last_quantity = st.ltq;
            t.oi = st.oi;
            t.total_buy_quantity = st.bids.iter().map(|l| l.quantity).sum();
            t.total_sell_quantity = st.asks.iter().map(|l| l.quantity).sum();
            t.derive_change();
            if t.change_percent == 0.0 && st.change_percent != 0.0 {
                t.change_percent = st.change_percent;
            }
        }
        t
    }

    fn depth(inst: &Inst) -> NormalizedDepth {
        let st = &inst.state;
        let mut buy = st.bids.clone();
        let mut sell = st.asks.clone();
        buy.resize(5, DepthLevel::default());
        sell.resize(5, DepthLevel::default());
        NormalizedDepth {
            symbol: inst.sub.symbol.clone(),
            exchange: inst.sub.exchange.clone(),
            ltp: st.ltp,
            total_buy_quantity: buy.iter().map(|l| l.quantity).sum(),
            total_sell_quantity: sell.iter().map(|l| l.quantity).sum(),
            buy,
            sell,
            timestamp_ms: now_ms(),
        }
    }

    /// Decode one binary frame.
    pub fn parse_binary(&mut self, raw: &[u8]) -> Vec<FeedEvent> {
        let Some(frame) = proto::decode_market(raw) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match frame {
            MarketFrame::Index(m) => {
                for x in m.indexes.iter().chain(m.instruments.iter()) {
                    let Some(key) = self.by_name.get(&feed_name(&x.indexname)).cloned() else {
                        continue;
                    };
                    let Some(inst) = self.insts.get_mut(&key) else {
                        continue;
                    };
                    let st = &mut inst.state;
                    st.has_index = true;
                    st.ltp = x.index_value as f64 / 100.0;
                    st.high = x.high_index_value as f64 / 100.0;
                    st.low = x.low_index_value as f64 / 100.0;
                    st.volume = x.volume;
                    st.prev_close = x.prev_close as f64 / 100.0;
                    st.change_percent = f64::from(x.changepercent);
                    if x.volume_oi != 0 {
                        st.oi = x.volume_oi;
                    }
                    if st.ltp > 0.0 {
                        out.push(FeedEvent::Tick(Self::tick(inst)));
                    }
                }
            }
            MarketFrame::Bucket(m) => {
                for x in m.indexes.iter().chain(m.instruments.iter()) {
                    let Some(key) = self.by_name.get(&feed_name(&x.indexname)).cloned() else {
                        continue;
                    };
                    let Some(inst) = self.insts.get_mut(&key) else {
                        continue;
                    };
                    let st = &mut inst.state;
                    if x.open != 0 {
                        st.open = x.open as f64 / 100.0;
                    }
                    if !st.has_index {
                        // The candle is the only source: refresh everything.
                        if x.close != 0 {
                            st.ltp = x.close as f64 / 100.0;
                        }
                        if x.high != 0 {
                            st.high = x.high as f64 / 100.0;
                        }
                        if x.low != 0 {
                            st.low = x.low as f64 / 100.0;
                        }
                        st.volume = if x.cumulative_volume != 0 {
                            x.cumulative_volume
                        } else {
                            x.bucket_volume
                        };
                        if st.ltp > 0.0 {
                            out.push(FeedEvent::Tick(Self::tick(inst)));
                        }
                    }
                }
            }
            MarketFrame::Orderbook(m) => {
                for x in &m.instruments {
                    let rid = if x.ref_id != 0 {
                        x.ref_id
                    } else {
                        i64::from(x.inst_id)
                    };
                    let Some(key) = self.by_ref.get(&rid).cloned() else {
                        continue;
                    };
                    let Some(inst) = self.insts.get_mut(&key) else {
                        continue;
                    };
                    let st = &mut inst.state;
                    if x.ltp != 0 {
                        st.ltp = x.ltp as f64 / 100.0;
                    }
                    if x.ltq != 0 {
                        st.ltq = x.ltq;
                    }
                    if x.volume != 0 {
                        st.volume = x.volume;
                    }
                    st.bids = levels(&x.bids);
                    st.asks = levels(&x.asks);
                    if st.ltp <= 0.0 {
                        continue;
                    }
                    out.push(FeedEvent::Tick(Self::tick(inst)));
                    if inst.sub.mode == FeedMode::Depth {
                        out.push(FeedEvent::Depth(Self::depth(inst)));
                    }
                }
            }
            MarketFrame::Greeks(m) => {
                for x in &m.instruments {
                    if let Some(key) = self.by_ref.get(&x.ref_id).cloned() {
                        if let Some(inst) = self.insts.get_mut(&key) {
                            inst.state.oi = x.oi;
                        }
                    }
                }
            }
        }
        out
    }
}

impl BrokerFeed for NubraFeed {
    fn broker(&self) -> &'static str {
        "nubra"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let mut req = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Nubra feed address is invalid".into()))?;
        let h = req.headers_mut();
        h.insert(
            "Authorization",
            format!("Bearer {}", self.token.expose())
                .parse()
                .map_err(|_| {
                    AppError::Auth("The Nubra session is not usable. Log in again.".into())
                })?,
        );
        h.insert(
            "x-device-id",
            DEVICE_ID
                .parse()
                .map_err(|_| AppError::Internal("Nubra device id is invalid".into()))?,
        );
        Ok(req)
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // Caches are per connection (web clears them on disconnect).
        for inst in self.insts.values_mut() {
            inst.state = State::default();
        }
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut added: Vec<Inst> = Vec::new();
        for s in subs {
            let key = s.instrument_key();
            self.forget(&key);
            let sub_name = subscription_name(s);
            let mut name_keys = vec![feed_name(&sub_name)];
            if is_index(&s.exchange) {
                let own = feed_name(&s.symbol);
                if !name_keys.contains(&own) {
                    name_keys.push(own);
                }
            }
            let ref_id = if is_index(&s.exchange) {
                None
            } else {
                mapping::ref_id(&s.token)
            };
            for n in &name_keys {
                self.by_name.insert(n.clone(), key.clone());
            }
            if let Some(r) = ref_id {
                self.by_ref.insert(r, key.clone());
            }
            let inst = Inst {
                sub: s.clone(),
                sub_name,
                name_keys,
                ws_ex: ws_exchange(&s.exchange),
                ref_id,
                state: State::default(),
            };
            self.insts.insert(key, inst.clone());
            added.push(inst);
        }
        let refs: Vec<&Inst> = added.iter().collect();
        self.frames("batch_subscribe", &refs)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let removed: Vec<Inst> = subs
            .iter()
            .filter_map(|s| self.forget(&s.instrument_key()))
            .collect();
        let refs: Vec<&Inst> = removed.iter().collect();
        self.frames("batch_unsubscribe", &refs)
    }

    fn mode_change_frames(
        &mut self,
        _old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        // Every mode rides the same channels; only the emitted shape changes.
        if let Some(inst) = self.insts.get_mut(&new.instrument_key()) {
            inst.sub = new.clone();
        }
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            Message::Text(t) if t.trim() == "Invalid Token" => {
                tracing::warn!("Nubra market feed refused the session");
                vec![FeedEvent::AuthFailed(
                    "Nubra refused the live market data session. Log in to Nubra again.".into(),
                )]
            }
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
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

// ---------------------------------------------------------------------------
// Order updates
// ---------------------------------------------------------------------------

/// `NubraIntentOrderStatus` -> OpenAlgo status (web `_STATUS_MAP`).
pub fn order_status(code: i64) -> &'static str {
    match code {
        2 => "complete",
        3 => "rejected",
        4 => "trigger pending",
        5 => "cancelled",
        6 => "expired",
        _ => "open",
    }
}

/// Decode one `NubraToClientIntentUpdate` frame (web
/// `NubraOrderUpdateAdapter.normalize`). Field numbers:
/// update.1 = response; response.1 intentOrderId, 2 status, 7 deliveryType
/// (1 CNC, 2 MIS), 8 priceType (1 LIMIT, 2 MARKET), 13 orderQty,
/// 14 filledQty, 17 price (paise), 18 average (paise), 19 tradeFill{2 price},
/// 25 refData{1 ref_id, 5 stock_name, 10 exchange, 11 derivativeType},
/// 29 side (1 BUY, 2 SELL), 31 rejection reason.
pub fn decode_order_update(raw: &[u8], symbols: &SymbolResolver) -> Option<OrderUpdate> {
    let (url, value) = proto::unwrap_any(raw)?;
    if !url.ends_with("NubraToClientIntentUpdate") {
        return None;
    }
    let update = proto::decode_fields(&value)?;
    let resp_bytes = proto::first_bytes(&update, 1).filter(|b| !b.is_empty())?;
    let resp = proto::decode_fields(resp_bytes)?;
    let status = order_status(proto::first_int(&resp, 2));
    let (mut symbol, mut exchange) = (String::new(), String::new());
    if let Some(rd) = proto::first_bytes(&resp, 25).and_then(proto::decode_fields) {
        let bs = proto::first_str(&rd, 5);
        let ex = proto::first_str(&rd, 10);
        let dt = proto::first_str(&rd, 11);
        let rid = proto::first_int(&rd, 1);
        let rid = if rid != 0 {
            rid.to_string()
        } else {
            String::new()
        };
        match mapping::resolve_instrument(symbols, &ex, &dt, &rid, &bs) {
            Some((s, e)) => {
                symbol = s;
                exchange = e;
            }
            None => {
                tracing::warn!("Nubra order update is not in the master contract");
                symbol = bs;
                exchange = mapping::map_exchange(&ex, &dt);
            }
        }
    }
    let qty = proto::first_int(&resp, 13);
    let filled = proto::first_int(&resp, 14);
    let mut avg = proto::first_int(&resp, 18);
    if let Some(fill) = proto::first_bytes(&resp, 19).and_then(proto::decode_fields) {
        let p = proto::first_int(&fill, 2);
        if p != 0 {
            avg = p;
        }
    }
    let oid = proto::first_int(&resp, 1);
    Some(OrderUpdate {
        orderid: if oid != 0 {
            oid.to_string()
        } else {
            String::new()
        },
        symbol,
        exchange,
        action: match proto::first_int(&resp, 29) {
            1 => "BUY",
            2 => "SELL",
            _ => "",
        }
        .to_string(),
        quantity: qty,
        price: proto::first_int(&resp, 17) as f64 / 100.0,
        trigger_price: 0.0,
        pricetype: match proto::first_int(&resp, 8) {
            1 => "LIMIT",
            2 => "MARKET",
            _ => "",
        }
        .to_string(),
        product: match proto::first_int(&resp, 7) {
            1 => "CNC",
            2 => "MIS",
            _ => "",
        }
        .to_string(),
        order_status: status.to_string(),
        filled_quantity: filled,
        pending_quantity: (qty - filled).max(0),
        average_price: avg as f64 / 100.0,
        rejection_reason: if status == "rejected" {
            proto::first_str(&resp, 31)
        } else {
            String::new()
        },
    })
}

/// How the relay reaches the order socket.
pub struct OrderUpstream {
    pub http: reqwest::Client,
    pub base_url: String,
    pub fallback_url: String,
    pub session: Secret,
}

enum UrlLookup {
    Url(String),
    Refused,
}

impl OrderUpstream {
    /// `GET /userinfo` -> `env_info.user_ws_url`, else the fallback.
    async fn ws_url(&self) -> UrlLookup {
        let resp = self
            .http
            .get(format!("{}/userinfo", self.base_url))
            .header("Authorization", format!("Bearer {}", self.session.expose()))
            .header("Accept", "application/json")
            .header("x-device-id", DEVICE_ID)
            .send()
            .await;
        match resp {
            Ok(r) if matches!(r.status().as_u16(), 401 | 403 | 440) => UrlLookup::Refused,
            Ok(r) if r.status().is_success() => {
                let v: Value = r.json().await.unwrap_or(Value::Null);
                match v
                    .get("env_info")
                    .and_then(|e| e.get("user_ws_url"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    Some(u) => UrlLookup::Url(u.to_string()),
                    None => UrlLookup::Url(self.fallback_url.clone()),
                }
            }
            _ => {
                tracing::warn!("Nubra /userinfo unavailable; using the default order socket");
                UrlLookup::Url(self.fallback_url.clone())
            }
        }
    }
}

const ORDER_FEED_REFUSED: &str = "Nubra refused the order update session. Log in to Nubra again.";

#[async_trait]
impl Upstream for OrderUpstream {
    fn broker(&self) -> &'static str {
        "nubra"
    }

    async fn open(&self) -> Open {
        let url = match self.ws_url().await {
            UrlLookup::Url(u) => u,
            UrlLookup::Refused => return Open::AuthFailed(ORDER_FEED_REFUSED.into()),
        };
        let Ok(mut req) = url.as_str().into_client_request() else {
            tracing::warn!("Nubra order socket address is invalid");
            return Open::Unavailable;
        };
        let (Ok(auth), Ok(dev)) = (
            format!("Bearer {}", self.session.expose()).parse(),
            DEVICE_ID.parse(),
        ) else {
            return Open::AuthFailed(ORDER_FEED_REFUSED.into());
        };
        req.headers_mut().insert("Authorization", auth);
        req.headers_mut().insert("x-device-id", dev);
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => Open::Ready(Box::new(ws)),
            Err(tokio_tungstenite::tungstenite::Error::Http(resp))
                if matches!(resp.status().as_u16(), 401 | 403 | 440) =>
            {
                Open::AuthFailed(ORDER_FEED_REFUSED.into())
            }
            Err(e) => {
                tracing::debug!(
                    "Nubra order socket connect failed: {}",
                    crate::brokers::common::redact::ws_error_kind(&e)
                );
                Open::Unavailable
            }
        }
    }

    fn session(&self) -> Box<dyn Session> {
        Box::new(OrderSession {
            token: self.session.clone(),
        })
    }
}

/// Relay-side protocol for the order socket.
pub struct OrderSession {
    token: Secret,
}

impl OrderSession {
    pub fn new(token: &str) -> Self {
        Self {
            token: Secret::new(token),
        }
    }
}

impl Session for OrderSession {
    fn on_open(&mut self) -> Vec<Message> {
        vec![Message::Text(format!(
            "subscribe {} notifications notification",
            self.token.expose()
        ))]
    }

    fn on_upstream(&mut self, msg: Message) -> Step {
        match msg {
            Message::Text(t) if t.trim() == "Invalid Token" => Step {
                auth_failed: Some(ORDER_FEED_REFUSED.into()),
                ..Default::default()
            },
            m @ Message::Binary(_) => Step {
                down: vec![m],
                ..Default::default()
            },
            _ => Step::default(),
        }
    }
}

/// The order-update feed (driven by the shared manager through the relay).
pub struct NubraOrderFeed {
    upstream: Arc<OrderUpstream>,
    relay: parking_lot::Mutex<Option<RelayHandle>>,
    symbols: SymbolResolver,
}

impl NubraOrderFeed {
    pub fn new(upstream: OrderUpstream, symbols: SymbolResolver) -> Self {
        Self {
            upstream: Arc::new(upstream),
            relay: parking_lot::Mutex::new(None),
            symbols,
        }
    }
}

impl BrokerFeed for NubraOrderFeed {
    fn broker(&self) -> &'static str {
        "nubra"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let up = self.upstream.clone();
        let url = relay::ensure_started(&self.relay, move || up as Arc<dyn Upstream>)?;
        url.as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Nubra order feed relay address is invalid".into()))
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
        match msg {
            Message::Text(t) => match relay::control(t) {
                Some(Ok(())) => vec![FeedEvent::AuthOk],
                Some(Err(m)) => vec![FeedEvent::AuthFailed(m)],
                None => Vec::new(),
            },
            Message::Binary(b) => decode_order_update(b, &self.symbols)
                .map(FeedEvent::OrderUpdate)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }
}
