//! Upstox Market Data Feed V3 and the portfolio order-update stream (web
//! `streaming/upstox_client.py`, `upstox_adapter.py`,
//! `upstox_order_adapter.py`).
//!
//! * Connect: `GET /v3/feed/market-data-feed/authorize` returns a single-use,
//!   signed `wss://` URL, fetched fresh on every (re)connect in
//!   `BrokerFeed::prepare`; the shared manager then connects to it. The
//!   signed query is never logged.
//! * Subscribe: a BINARY frame holding JSON
//!   `{"guid","method":"sub"|"unsub","data":{"instrumentKeys":[..],"mode"}}`;
//!   OpenAlgo mode 1 is `ltpc`, modes 2 and 3 are `full` (5-level depth;
//!   `full_d30` is Plus-only and never sent). Per-mode key caps are
//!   enforced client side.
//! * Frames: protobuf `FeedResponse` (`super::proto`); prices are rupees.
//!   The feeds map is keyed by instrument key, matched to subscriptions
//!   exactly or by the part after `|`. The last LTPC per instrument is kept
//!   so a frame without one still carries a price.
//! * Liveness: Upstox sends no application heartbeat; the manager pings
//!   every 30 s and the pongs reach its watchdog as heartbeats.

use super::mapping::{oa_symbol, order_update_status};
use super::proto::{feed::FeedUnion, full_feed::FullFeedUnion, FeedResponse, Ltpc, MarketOhlc};
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, PrepareError, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::Secret;
use async_trait::async_trait;
use prost::Message as _;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Authorize call budget (web `HTTP_TIMEOUT`).
const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(10);
/// Client ping period (web `ping_interval=30`).
const PING_PERIOD: Duration = Duration::from_secs(30);

/// Keys one connection may carry per wire mode (web `MODE_KEY_LIMITS`).
pub fn mode_key_limit(mode: &str) -> usize {
    match mode {
        "ltpc" => 5000,
        "option_greeks" => 3000,
        "full" => 2000,
        "full_d30" => 50,
        _ => 2000,
    }
}

/// Budget once more than one mode is live (web `MODE_COMBINED_LIMITS`).
pub fn mode_combined_limit(mode: &str) -> usize {
    match mode {
        "ltpc" | "option_greeks" => 2000,
        _ => 1500,
    }
}

/// OpenAlgo mode -> Upstox wire mode.
pub fn wire_mode(mode: FeedMode) -> &'static str {
    match mode {
        FeedMode::Ltp => "ltpc",
        FeedMode::Quote | FeedMode::Depth => "full",
    }
}

/// `brexchange|token-after-the-bar` (web `_create_instrument_key`).
pub fn instrument_key(brexchange: &str, token: &str) -> String {
    let t = token.rsplit('|').next().unwrap_or(token);
    format!("{}|{}", brexchange, t)
}

fn guid() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..20].to_string()
}

/// One subscribe / unsubscribe frame.
pub fn sub_frame(method: &str, keys: &[String], mode: Option<&str>) -> Message {
    let mut data = json!({ "instrumentKeys": keys });
    if let (Some(m), "sub") = (mode, method) {
        data["mode"] = json!(m);
    }
    let v = json!({"guid": guid(), "method": method, "data": data});
    Message::Binary(v.to_string().into_bytes())
}

/// URL without its query, for logs (the authorized URL is signed).
fn redact(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

async fn authorize(
    http: &reqwest::Client,
    url: &str,
    token: &Secret,
) -> std::result::Result<String, u16> {
    let resp = http
        .get(url)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {}", token.expose()))
        .timeout(AUTHORIZE_TIMEOUT)
        .send()
        .await
        .map_err(|e| {
            tracing::debug!("Upstox feed authorize failed: {}", e);
            0u16
        })?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let code = super::mapping::error_code(&body).unwrap_or_default();
        if status.as_u16() == 429 || code.contains("limit") {
            tracing::error!(
                "Upstox refused the feed authorization; Upstox allows 2 market data connections on Standard and 5 on Plus"
            );
        } else if status.as_u16() != 401 {
            tracing::warn!(status = status.as_u16(), code = %code, "Upstox feed authorize refused");
        }
        return Err(status.as_u16());
    }
    body.pointer("/data/authorized_redirect_uri")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            tracing::error!("Upstox feed authorize returned no socket address");
            0
        })
}

fn refused_login() -> String {
    "Upstox did not accept the stored login for live data. Log in to Upstox again.".into()
}

fn invalid_address() -> AppError {
    AppError::Broker("Upstox sent a live data address OpenAlgo could not use. Try again.".into())
}

/// An authorize endpoint and the token it is called with. Each call hands
/// out a single-use signed socket URL, so it runs before every connect.
#[derive(Clone)]
pub struct Authorizer {
    http: reqwest::Client,
    url: String,
    token: Secret,
}

impl Authorizer {
    /// Market data (`/v3/feed/market-data-feed/authorize`).
    pub fn market(http: reqwest::Client, api_base: &str, token: &str) -> Self {
        Self {
            http,
            url: format!("{}/v3/feed/market-data-feed/authorize", api_base),
            token: Secret::new(token),
        }
    }

    /// Order updates (`/v2/feed/portfolio-stream-feed/authorize`).
    pub fn orders(http: reqwest::Client, api_base: &str, token: &str) -> Self {
        Self {
            http,
            url: format!(
                "{}/v2/feed/portfolio-stream-feed/authorize?update_types=order",
                api_base
            ),
            token: Secret::new(token),
        }
    }

    async fn signed_url(&self) -> std::result::Result<String, u16> {
        authorize(&self.http, &self.url, &self.token).await
    }
}

// ---------------------------------------------------------------------------
// Market data feed
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    token: String,
    mode: FeedMode,
}

/// The Upstox market-data `BrokerFeed`.
pub struct UpstoxFeed {
    authorizer: Authorizer,
    /// Signed socket URL from the last `prepare` (single use).
    socket_url: Option<Secret>,
    /// Instrument key -> subscription (bounded by live subscriptions).
    subs: HashMap<String, SubInfo>,
    /// Keys live on the current connection, per wire mode.
    live: HashMap<&'static str, HashSet<String>>,
    /// Last LTPC per instrument key (removed with the subscription).
    last_ltpc: HashMap<String, Ltpc>,
}

impl UpstoxFeed {
    pub fn new(authorizer: Authorizer, _symbols: SymbolResolver) -> Self {
        Self {
            authorizer,
            socket_url: None,
            subs: HashMap::new(),
            live: HashMap::new(),
            last_ltpc: HashMap::new(),
        }
    }

    /// Keys of `mode` that fit the per-mode and combined caps (web
    /// `_enforce_key_limits`); the rest are dropped with an error log.
    fn admit(&mut self, mode: &'static str, keys: Vec<String>) -> Vec<String> {
        let already = self.live.get(mode).cloned().unwrap_or_default();
        let mut seen = HashSet::new();
        let mut new_keys: Vec<String> = keys
            .into_iter()
            .filter(|k| !already.contains(k) && seen.insert(k.clone()))
            .collect();
        let limit = mode_key_limit(mode);
        let room = limit.saturating_sub(already.len());
        if new_keys.len() > room {
            tracing::error!(
                "Upstox {} subscription cap reached ({} per connection); dropping {} instrument(s)",
                mode,
                limit,
                new_keys.len() - room
            );
            new_keys.truncate(room);
        }
        let mut modes: HashSet<&str> = self
            .live
            .keys()
            .copied()
            .filter(|m| self.live.get(m).is_some_and(|s| !s.is_empty()))
            .collect();
        modes.insert(mode);
        if modes.len() > 1 {
            let budget = modes
                .iter()
                .map(|m| mode_combined_limit(m))
                .min()
                .unwrap_or(1500);
            let total: usize = self.live.values().map(HashSet::len).sum();
            let room = budget.saturating_sub(total);
            if new_keys.len() > room {
                tracing::error!(
                    "Upstox combined subscription cap reached ({} across modes); dropping {} instrument(s)",
                    budget,
                    new_keys.len() - room
                );
                new_keys.truncate(room);
            }
        }
        self.live
            .entry(mode)
            .or_default()
            .extend(new_keys.iter().cloned());
        new_keys
    }

    fn sub_frames_for(&mut self, mode: &'static str, keys: Vec<String>) -> Vec<Message> {
        let keys = self.admit(mode, keys);
        keys.chunks(mode_key_limit(mode))
            .map(|c| sub_frame("sub", c, Some(mode)))
            .collect()
    }

    fn find_sub(&self, feed_key: &str) -> Option<(&String, &SubInfo)> {
        if let Some(kv) = self.subs.get_key_value(feed_key) {
            return Some(kv);
        }
        let token = feed_key.rsplit('|').next().unwrap_or(feed_key);
        self.subs
            .iter()
            .find(|(_, s)| s.token == token || s.token == feed_key)
    }

    /// Decode one protobuf frame into events.
    pub fn parse_frame(&mut self, bytes: &[u8]) -> Vec<FeedEvent> {
        let resp = match FeedResponse::decode(bytes) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Upstox feed frame could not be decoded: {}", e);
                return Vec::new();
            }
        };
        if resp.r#type == super::proto::Type::MarketInfo as i32 {
            if let Some(info) = &resp.market_info {
                tracing::debug!(
                    "Upstox market status update for {} segment(s)",
                    info.segment_status.len()
                );
            }
            return Vec::new();
        }
        let mut out = Vec::new();
        for (key, feed) in resp.feeds {
            let Some((ik, sub)) = self.find_sub(&key).map(|(k, s)| (k.clone(), s.clone())) else {
                tracing::debug!("No Upstox subscription for feed key {}", key);
                continue;
            };
            if let Some(l) = tick_ltpc(&feed.feed_union) {
                self.last_ltpc.insert(ik.clone(), l);
            }
            let cached = self.last_ltpc.get(&ik).cloned();
            out.extend(normalise(&sub, feed.feed_union.as_ref(), cached.as_ref()));
        }
        out
    }
}

/// The LTPC a feed carries, whichever branch it came in.
fn tick_ltpc(f: &Option<FeedUnion>) -> Option<Ltpc> {
    match f.as_ref()? {
        FeedUnion::Ltpc(l) => Some(l.clone()),
        FeedUnion::FullFeed(ff) => match ff.full_feed_union.as_ref()? {
            FullFeedUnion::MarketFf(m) => m.ltpc.clone(),
            FullFeedUnion::IndexFf(i) => i.ltpc.clone(),
        },
        FeedUnion::FirstLevelWithGreeks(g) => g.ltpc.clone(),
    }
}

/// The `1d` OHLC, else the first one (web `_extract_quote_data`).
fn day_ohlc(m: Option<&MarketOhlc>) -> Option<super::proto::Ohlc> {
    let list = &m?.ohlc;
    list.iter()
        .find(|o| o.interval == "1d")
        .or_else(|| list.first())
        .cloned()
}

fn base_tick(sub: &SubInfo) -> NormalizedTick {
    NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode: sub.mode.code(),
        timestamp_ms: now_ms(),
        ..Default::default()
    }
}

fn apply_ltpc(t: &mut NormalizedTick, l: &Ltpc) {
    t.ltp = l.ltp;
    t.last_quantity = l.ltq;
    t.last_trade_time_ms = l.ltt;
    // `cp` is the previous close.
    t.close = l.cp;
}

/// Normalise one feed for one subscription (web `_extract_market_data`).
#[cfg(test)]
pub(crate) fn normalise_for_test(
    symbol: &str,
    exchange: &str,
    mode: FeedMode,
    feed: Option<&FeedUnion>,
    cached: Option<&Ltpc>,
) -> Vec<FeedEvent> {
    let sub = SubInfo {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: String::new(),
        mode,
    };
    normalise(&sub, feed, cached)
}

fn normalise(sub: &SubInfo, feed: Option<&FeedUnion>, cached: Option<&Ltpc>) -> Vec<FeedEvent> {
    let mut t = base_tick(sub);
    match sub.mode {
        FeedMode::Ltp => {
            match feed {
                Some(FeedUnion::Ltpc(l)) => apply_ltpc(&mut t, l),
                _ => match cached {
                    // web: an extractor that found nothing falls back to the
                    // cached LTPC.
                    Some(c) => apply_ltpc(&mut t, c),
                    None => return Vec::new(),
                },
            }
            if t.ltp == 0.0 {
                if let Some(c) = cached {
                    t.ltp = c.ltp;
                }
            }
            t.derive_change();
            vec![FeedEvent::Tick(t)]
        }
        FeedMode::Quote | FeedMode::Depth => {
            let full = match feed {
                Some(FeedUnion::FullFeed(ff)) => ff.full_feed_union.as_ref(),
                _ => None,
            };
            let Some(full) = full else {
                // No full feed in this frame: publish the cached trade fields.
                let Some(c) = cached else {
                    return Vec::new();
                };
                apply_ltpc(&mut t, c);
                t.derive_change();
                return vec![FeedEvent::Tick(t)];
            };
            let (ltpc, ohlc, market) = match full {
                FullFeedUnion::MarketFf(m) => {
                    (m.ltpc.as_ref(), day_ohlc(m.market_ohlc.as_ref()), Some(m))
                }
                FullFeedUnion::IndexFf(i) => {
                    (i.ltpc.as_ref(), day_ohlc(i.market_ohlc.as_ref()), None)
                }
            };
            if let Some(l) = ltpc {
                apply_ltpc(&mut t, l);
            }
            if t.ltp == 0.0 {
                if let Some(c) = cached {
                    t.ltp = c.ltp;
                }
            }
            if let Some(o) = &ohlc {
                t.open = o.open;
                t.high = o.high;
                t.low = o.low;
                t.volume = o.vol;
                if t.close == 0.0 {
                    t.close = o.close;
                }
            }
            if let Some(m) = market {
                t.average_price = m.atp;
                t.total_buy_quantity = m.tbq as i64;
                t.total_sell_quantity = m.tsq as i64;
                t.oi = m.oi as i64;
            }
            t.derive_change();
            let mut out = Vec::with_capacity(2);
            if sub.mode == FeedMode::Depth {
                let mut buy: Vec<DepthLevel> = Vec::new();
                let mut sell: Vec<DepthLevel> = Vec::new();
                if let Some(level) = market.and_then(|m| m.market_level.as_ref()) {
                    for q in &level.bid_ask_quote {
                        if q.bid_p > 0.0 {
                            buy.push(DepthLevel {
                                price: q.bid_p,
                                quantity: q.bid_q,
                                orders: 0,
                            });
                        }
                        if q.ask_p > 0.0 {
                            sell.push(DepthLevel {
                                price: q.ask_p,
                                quantity: q.ask_q,
                                orders: 0,
                            });
                        }
                    }
                }
                buy.sort_by(|a, b| b.price.total_cmp(&a.price));
                sell.sort_by(|a, b| a.price.total_cmp(&b.price));
                buy.resize(5, DepthLevel::default());
                sell.resize(5, DepthLevel::default());
                let depth = NormalizedDepth {
                    symbol: t.symbol.clone(),
                    exchange: t.exchange.clone(),
                    ltp: t.ltp,
                    buy,
                    sell,
                    total_buy_quantity: t.total_buy_quantity,
                    total_sell_quantity: t.total_sell_quantity,
                    timestamp_ms: t.timestamp_ms,
                };
                out.push(FeedEvent::Tick(t));
                out.push(FeedEvent::Depth(depth));
            } else {
                out.push(FeedEvent::Tick(t));
            }
            out
        }
    }
}

#[async_trait]
impl BrokerFeed for UpstoxFeed {
    fn broker(&self) -> &'static str {
        "upstox"
    }

    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        self.socket_url = None;
        match self.authorizer.signed_url().await {
            Ok(url) => {
                tracing::debug!("Upstox feed socket: {}", redact(&url));
                self.socket_url = Some(Secret::new(url));
                Ok(())
            }
            Err(401) => Err(PrepareError::AuthFailed(refused_login())),
            Err(_) => Err(PrepareError::Unavailable),
        }
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let url = self.socket_url.as_ref().ok_or_else(invalid_address)?;
        url.expose().into_client_request().map_err(|_| {
            tracing::error!(
                "Upstox feed socket address is invalid: {}",
                redact(url.expose())
            );
            invalid_address()
        })
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // A new socket carries no subscriptions; the manager re-sends them.
        self.live.clear();
        Vec::new()
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((PING_PERIOD, Message::Ping(Vec::new())))
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut by_mode: Vec<(&'static str, Vec<String>)> = Vec::new();
        for s in subs {
            if s.depth > 5 {
                tracing::warn!(
                    "Upstox streams 5 depth levels; {} level(s) requested for {} are served as 5",
                    s.depth,
                    s.symbol
                );
            }
            let ik = instrument_key(&s.brexchange, &s.token);
            self.subs.insert(
                ik.clone(),
                SubInfo {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    token: s.token.rsplit('|').next().unwrap_or(&s.token).to_string(),
                    mode: s.mode,
                },
            );
            let m = wire_mode(s.mode);
            match by_mode.iter_mut().find(|(x, _)| *x == m) {
                Some((_, v)) => v.push(ik),
                None => by_mode.push((m, vec![ik])),
            }
        }
        let mut out = Vec::new();
        for (mode, keys) in by_mode {
            out.extend(self.sub_frames_for(mode, keys));
        }
        out
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut keys = Vec::new();
        for s in subs {
            let ik = instrument_key(&s.brexchange, &s.token);
            self.subs.remove(&ik);
            self.last_ltpc.remove(&ik);
            for set in self.live.values_mut() {
                set.remove(&ik);
            }
            keys.push(ik);
        }
        if keys.is_empty() {
            return Vec::new();
        }
        vec![sub_frame("unsub", &keys, None)]
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        if wire_mode(old.mode) == wire_mode(new.mode) {
            // Quote <-> depth share the `full` stream: only the
            // normalisation changes.
            let ik = instrument_key(&new.brexchange, &new.token);
            if let Some(s) = self.subs.get_mut(&ik) {
                s.mode = new.mode;
            }
            return Vec::new();
        }
        let mut v = self.unsubscribe_frames(std::slice::from_ref(old));
        v.extend(self.subscribe_frames(std::slice::from_ref(new)));
        v
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_frame(b),
            Message::Text(t) => parse_text(t),
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }
}

/// Upstox's JSON status frames (logged; they carry no market data).
fn parse_text(t: &str) -> Vec<FeedEvent> {
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        if v.get("status").and_then(Value::as_str) == Some("failed") {
            let method = v.get("method").and_then(Value::as_str).unwrap_or("request");
            let error = v.get("error").map(|e| e.to_string()).unwrap_or_default();
            tracing::error!("Upstox feed {} failed: {}", method, error);
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Order updates (portfolio stream)
// ---------------------------------------------------------------------------

/// Where the order stream connects after `prepare`.
enum OrderTarget {
    /// The single-use signed URL from the authorize call.
    Signed(Secret),
    /// The direct endpoint with the Bearer header (authorize failed).
    Direct,
}

/// The Upstox order-update `BrokerFeed`: no subscriptions, JSON text
/// frames normalised to `OrderUpdate`. `prepare` authorizes (single-use
/// code, so never cached), else falls back to the direct endpoint with the
/// Bearer header, like the web.
pub struct UpstoxOrderFeed {
    authorizer: Authorizer,
    direct_url: String,
    target: Option<OrderTarget>,
    symbols: SymbolResolver,
}

impl UpstoxOrderFeed {
    pub fn new(authorizer: Authorizer, api_base: &str, symbols: SymbolResolver) -> Self {
        let ws_base = api_base
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        Self {
            authorizer,
            direct_url: format!(
                "{}/v2/feed/portfolio-stream-feed?update_types=order",
                ws_base
            ),
            target: None,
            symbols,
        }
    }
}

/// web `UpstoxOrderUpdateAdapter.normalize`; `None` for non-order updates.
pub fn normalize_order_update(v: &Value, symbols: &SymbolResolver) -> Option<OrderUpdate> {
    match v.get("update_type").and_then(Value::as_str) {
        None | Some("order") => {}
        Some(_) => return None,
    }
    let s = |k: &str| match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    let f = |k: &str| match v.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(t)) => t.parse().unwrap_or(0.0),
        _ => 0.0,
    };
    let i = |k: &str| f(k) as i64;
    let status = order_update_status(&s("status"));
    let quantity = i("quantity");
    let filled = i("filled_quantity");
    let exchange = s("exchange");
    let br = {
        let a = s("trading_symbol");
        if a.is_empty() {
            s("tradingsymbol")
        } else {
            a
        }
    };
    let token = {
        let a = s("instrument_token");
        if a.is_empty() {
            s("instrument_key")
        } else {
            a
        }
    };
    let pending = match i("pending_quantity") {
        0 => (quantity - filled).max(0),
        p => p,
    };
    let product = match s("product").as_str() {
        "D" => "CNC".to_string(),
        "I" => "MIS".to_string(),
        other => other.to_string(),
    };
    let rejection_reason = if status == "rejected" {
        let m = s("status_message");
        if m.is_empty() {
            s("status")
        } else {
            m
        }
    } else {
        String::new()
    };
    Some(OrderUpdate {
        orderid: s("order_id"),
        symbol: if exchange.is_empty() {
            br.clone()
        } else {
            oa_symbol(symbols, &token, &exchange, &br)
        },
        exchange,
        action: s("transaction_type"),
        quantity,
        price: f("price"),
        trigger_price: f("trigger_price"),
        pricetype: s("order_type"),
        product,
        order_status: status,
        filled_quantity: filled,
        pending_quantity: pending,
        average_price: f("average_price"),
        rejection_reason,
    })
}

#[async_trait]
impl BrokerFeed for UpstoxOrderFeed {
    fn broker(&self) -> &'static str {
        "upstox"
    }

    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        self.target = Some(match self.authorizer.signed_url().await {
            Ok(url) => OrderTarget::Signed(Secret::new(url)),
            Err(401) => return Err(PrepareError::AuthFailed(refused_login())),
            Err(_) => {
                tracing::warn!("Upstox order stream authorize failed; using the direct endpoint");
                OrderTarget::Direct
            }
        });
        Ok(())
    }

    fn ws_request(&self) -> Result<WsRequest> {
        match self.target.as_ref().ok_or_else(invalid_address)? {
            OrderTarget::Signed(url) => url
                .expose()
                .into_client_request()
                .map_err(|_| invalid_address()),
            OrderTarget::Direct => {
                let mut r = self
                    .direct_url
                    .as_str()
                    .into_client_request()
                    .map_err(|_| invalid_address())?;
                let h = format!("Bearer {}", self.authorizer.token.expose())
                    .parse()
                    .map_err(|_| invalid_address())?;
                r.headers_mut().insert("Authorization", h);
                Ok(r)
            }
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((PING_PERIOD, Message::Ping(Vec::new())))
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        let text = match msg {
            Message::Text(t) => t.clone(),
            Message::Binary(b) => String::from_utf8_lossy(b).into_owned(),
            Message::Ping(_) | Message::Pong(_) => return vec![FeedEvent::Heartbeat],
            _ => return Vec::new(),
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return Vec::new();
        };
        normalize_order_update(&v, &self.symbols)
            .map(|u| vec![FeedEvent::OrderUpdate(u)])
            .unwrap_or_default()
    }
}
