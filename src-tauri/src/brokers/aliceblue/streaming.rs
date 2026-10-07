//! AliceBlue market-data (Noren JSON) and order-status sockets.
//!
//! Market data (web `api/alicebluewebsocket.py`, `streaming/aliceblue_adapter.py`,
//! `streaming/aliceblue_mapping.py`):
//! * before every connect: `POST /open-api/od/v1/profile/invalidateWsSess`
//!   then `createWsSess`, body `{"source":"API","userId":<UCC>}`;
//! * `wss://ws1.aliceblueonline.com/NorenWS/` (then `ws2`), first frame
//!   `{"susertoken": SHA256(SHA256(JWT)), "t":"c", "actid":"<UCC>_API",
//!   "uid":"<UCC>_API", "source":"API"}`, answered by `{"t":"ck","s":"OK"}`
//!   (or the documented `{"t":"cf","k":"OK"}`);
//! * `{"t":"t"|"d","k":"NSE|2885#NFO|54957"}` subscribes ticks / depth,
//!   `{"t":"u","k":..}` unsubscribes, `{"k":"","t":"h"}` keeps it alive;
//! * frames `tk` (tick snapshot) / `tf` (tick delta) / `dk` (depth
//!   snapshot) / `df` (depth delta) with keys `e, tk, ts, lp, o, h, l, c, v,
//!   pc, cv, ap, ft, ltq, tbq, tsq, oi, poi, toi, bp1..5, bq1..5, bo1..5,
//!   sp1..5, sq1..5, so1..5` (web `alicebluewebsocket.py:476-741`).
//!   Deltas send 0 for unchanged prices, so the snapshot keeps the last
//!   non-zero value (web `aliceblue_adapter.py:_update_market_snapshot`).
//!
//! Order Status Feed (web `aliceblue_order_adapter.py`): `GET
//! /open-api/order-notify/ws/createWsToken` -> `result[0].orderToken`,
//! connect `wss://a3.aliceblueonline.com/open-api/order-notify/websocket`,
//! send `{"orderToken","userId"}`, heartbeat `{"heartbeat":"h","userId"}`
//! every 55 s, `t == "om"` frames carry Noren order fields.
//!
//! Both connects need REST calls first, which `BrokerFeed::ws_request`
//! cannot make, so both run through the loopback relay
//! (`crate::brokers::upstox::relay`).

use super::mapping::{self, int, num, s};
use super::{text, Endpoints};
use crate::brokers::common::streaming::{
    now_ms, round2, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::brokers::upstox::relay::{self, Open, RelayHandle, Session, Step, Upstream};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Keepalive period on the market socket (adapter 30 s; docs ask for one
/// within every 50 s).
pub const HEARTBEAT: Duration = Duration::from_secs(30);
/// Order-feed heartbeat (under AliceBlue's 60 s timeout).
pub const ORDER_HEARTBEAT: Duration = Duration::from_secs(55);
/// Budget of each REST call made before a connect.
const PREP_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// `SHA256(SHA256(JWT))`, hex (web `alicebluewebsocket.py:81-82`).
pub fn susertoken(jwt: &str) -> String {
    super::auth::sha256_hex(&super::auth::sha256_hex(jwt))
}

/// The login frame sent right after the socket opens.
pub fn connect_frame(jwt: &str, ucc: &str) -> String {
    json!({
        "susertoken": susertoken(jwt),
        "t": "c",
        "actid": format!("{}_API", ucc),
        "uid": format!("{}_API", ucc),
        "source": "API",
    })
    .to_string()
}

/// `{"t":"t"|"d","k":"NSE|2885#NFO|54957"}`.
pub fn subscribe_frame(keys: &[String], depth: bool) -> String {
    json!({"t": if depth { "d" } else { "t" }, "k": keys.join("#")}).to_string()
}

/// `{"t":"u","k":..}`.
pub fn unsubscribe_frame(keys: &[String]) -> String {
    json!({"t": "u", "k": keys.join("#")}).to_string()
}

pub fn heartbeat_frame() -> String {
    json!({"k": "", "t": "h"}).to_string()
}

/// `Some(true)` / `Some(false)` when the frame answers the login, `None`
/// for any other frame (web `on_message` and adapter `_handle_message`).
pub fn auth_ack(v: &Value) -> Option<bool> {
    match s(v, "t").as_str() {
        "ck" => Some(s(v, "s").eq_ignore_ascii_case("ok")),
        "cf" => Some(s(v, "k").eq_ignore_ascii_case("ok")),
        _ if s(v, "s").eq_ignore_ascii_case("ok") => Some(true),
        _ => None,
    }
}

/// AliceBlue socket exchange for an OpenAlgo exchange
/// (web `AliceBlueExchangeMapper`).
pub fn ab_exchange(exchange: &str) -> &str {
    match exchange {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        "MCX_INDEX" => "MCX",
        other => other,
    }
}

/// Socket token for a master token: integer form, and index tokens lose a
/// leading `999` (web `aliceblue_adapter.py` subscribe).
pub fn feed_token(exchange: &str, token: &str) -> String {
    let t = mapping::normalize_token(token);
    if exchange.ends_with("_INDEX") && t.starts_with("999") && t.len() > 3 {
        t[3..].to_string()
    } else {
        t
    }
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

/// Tick snapshot as the REST quote path keeps it (web
/// `_process_tick_data`): `tk` replaces it, `tf` updates only the fields it
/// carries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuoteSnap {
    pub ltp: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub ltq: i64,
    pub avg_price: f64,
    pub oi: i64,
    pub total_buy_qty: i64,
    pub total_sell_qty: i64,
    pub bid: f64,
    pub ask: f64,
    pub bid_qty: i64,
    pub ask_qty: i64,
    /// A full `tk` snapshot has been seen.
    pub full: bool,
}

impl QuoteSnap {
    pub fn apply(&mut self, v: &Value) {
        let has = |k: &str| v.get(k).is_some();
        match s(v, "t").as_str() {
            "tk" => {
                *self = QuoteSnap {
                    ltp: num(v.get("lp")),
                    open: num(v.get("o")),
                    high: num(v.get("h")),
                    low: num(v.get("l")),
                    close: num(v.get("c")),
                    volume: int(v.get("v")),
                    ltq: int(v.get("ltq")),
                    avg_price: num(v.get("ap")),
                    oi: int(v.get("oi")),
                    total_buy_qty: int(v.get("tbq")),
                    total_sell_qty: int(v.get("tsq")),
                    bid: num(v.get("bp1")),
                    ask: num(v.get("sp1")),
                    bid_qty: int(v.get("bq1")),
                    ask_qty: int(v.get("sq1")),
                    full: true,
                };
            }
            "tf" => {
                if has("lp") {
                    self.ltp = num(v.get("lp"));
                }
                if has("v") {
                    self.volume = int(v.get("v"));
                }
                if has("ltq") {
                    self.ltq = int(v.get("ltq"));
                }
                if has("bp1") {
                    self.bid = num(v.get("bp1"));
                }
                if has("sp1") {
                    self.ask = num(v.get("sp1"));
                }
                if has("bq1") {
                    self.bid_qty = int(v.get("bq1"));
                }
                if has("sq1") {
                    self.ask_qty = int(v.get("sq1"));
                }
                if has("tbq") {
                    self.total_buy_qty = int(v.get("tbq"));
                }
                if has("tsq") {
                    self.total_sell_qty = int(v.get("tsq"));
                }
                if has("oi") {
                    self.oi = int(v.get("oi"));
                }
            }
            _ => {}
        }
    }
}

/// Depth snapshot (web `_process_depth_data`): `dk` rebuilds it from the
/// levels with a price, `df` updates level by level.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DepthSnap {
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
    pub ltp: f64,
    pub ltq: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub oi: i64,
    pub total_buy_qty: i64,
    pub total_sell_qty: i64,
}

fn merge_levels(levels: &mut Vec<DepthLevel>, v: &Value, p: &str, q: &str, o: &str) {
    for i in 1..=5 {
        let (pk, qk, ok) = (
            format!("{}{}", p, i),
            format!("{}{}", q, i),
            format!("{}{}", o, i),
        );
        if v.get(&pk).is_none() && v.get(&qk).is_none() && v.get(&ok).is_none() {
            continue;
        }
        while levels.len() < i {
            levels.push(DepthLevel::default());
        }
        let l = &mut levels[i - 1];
        if v.get(&pk).is_some() {
            l.price = num(v.get(&pk));
        }
        if v.get(&qk).is_some() {
            l.quantity = int(v.get(&qk));
        }
        if v.get(&ok).is_some() {
            l.orders = int(v.get(&ok));
        }
    }
}

fn full_levels(v: &Value, p: &str, q: &str, o: &str) -> Vec<DepthLevel> {
    (1..=5)
        .map(|i| DepthLevel {
            price: num(v.get(format!("{}{}", p, i).as_str())),
            quantity: int(v.get(format!("{}{}", q, i).as_str())),
            orders: int(v.get(format!("{}{}", o, i).as_str())),
        })
        .filter(|l| l.price > 0.0)
        .collect()
}

impl DepthSnap {
    pub fn apply(&mut self, v: &Value) {
        let has = |k: &str| v.get(k).is_some();
        match s(v, "t").as_str() {
            "dk" => {
                *self = DepthSnap {
                    bids: full_levels(v, "bp", "bq", "bo"),
                    asks: full_levels(v, "sp", "sq", "so"),
                    ltp: num(v.get("lp")),
                    ltq: int(v.get("ltq")),
                    open: num(v.get("o")),
                    high: num(v.get("h")),
                    low: num(v.get("l")),
                    close: num(v.get("c")),
                    volume: int(v.get("v")),
                    oi: int(v.get("oi")),
                    total_buy_qty: int(v.get("tbq")),
                    total_sell_qty: int(v.get("tsq")),
                };
            }
            "df" => {
                for (k, f) in [
                    ("lp", &mut self.ltp),
                    ("o", &mut self.open),
                    ("h", &mut self.high),
                    ("l", &mut self.low),
                    ("c", &mut self.close),
                ] {
                    if has(k) {
                        *f = num(v.get(k));
                    }
                }
                for (k, f) in [
                    ("v", &mut self.volume),
                    ("ltq", &mut self.ltq),
                    ("tbq", &mut self.total_buy_qty),
                    ("tsq", &mut self.total_sell_qty),
                    ("oi", &mut self.oi),
                ] {
                    if has(k) {
                        *f = int(v.get(k));
                    }
                }
                merge_levels(&mut self.bids, v, "bp", "bq", "bo");
                merge_levels(&mut self.asks, v, "sp", "sq", "so");
            }
            _ => {}
        }
    }
}

/// Live-feed snapshot with the adapter's retention rule: prices keep the
/// last non-zero value, volumes take any non-negative value, depth sides
/// are merged level by level and only levels with a price are published.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedSnap {
    pub ltp: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub avg_price: f64,
    pub volume: i64,
    pub ltq: i64,
    pub total_buy_qty: i64,
    pub total_sell_qty: i64,
    pub oi: i64,
    pub change_percent: Option<f64>,
    pub change: Option<f64>,
    pub ft_ms: i64,
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
}

impl FeedSnap {
    pub fn apply(&mut self, v: &Value) {
        for (k, f) in [
            ("lp", &mut self.ltp),
            ("o", &mut self.open),
            ("h", &mut self.high),
            ("l", &mut self.low),
            ("c", &mut self.close),
            ("ap", &mut self.avg_price),
        ] {
            if v.get(k).is_some() {
                let x = num(v.get(k));
                if x != 0.0 {
                    *f = x;
                }
            }
        }
        for (k, f) in [
            ("v", &mut self.volume),
            ("ltq", &mut self.ltq),
            ("tbq", &mut self.total_buy_qty),
            ("tsq", &mut self.total_sell_qty),
            ("toi", &mut self.oi),
        ] {
            if v.get(k).is_some() {
                let x = int(v.get(k));
                if x >= 0 {
                    *f = x;
                }
            }
        }
        if v.get("pc").is_some() {
            self.change_percent = Some(num(v.get("pc")));
        }
        if v.get("cv").is_some() {
            self.change = Some(num(v.get("cv")));
        }
        if v.get("ft").is_some() {
            let ft = int(v.get("ft"));
            if ft > 0 {
                self.ft_ms = if ft > 100_000_000_000 { ft } else { ft * 1000 };
            }
        }
        merge_levels(&mut self.bids, v, "bp", "bq", "bo");
        merge_levels(&mut self.asks, v, "sp", "sq", "so");
    }

    fn side(levels: &[DepthLevel]) -> Vec<DepthLevel> {
        levels
            .iter()
            .filter(|l| l.price != 0.0)
            .map(|l| DepthLevel {
                price: l.price,
                quantity: l.quantity,
                // The web publishes 0: AliceBlue's order counts are not
                // forwarded (aliceblue_adapter.py `_on_data_received`).
                orders: 0,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Connect preparation (shared with the pooled quote socket)
// ---------------------------------------------------------------------------

/// Outcome of the REST calls before a market-data connect.
#[derive(Debug, Clone, PartialEq)]
pub enum Prep {
    Ok,
    /// The broker refused the session (trader-facing message).
    Refused(String),
    /// Network or broker trouble; retry later.
    Unavailable,
}

fn refused_session() -> String {
    "AliceBlue refused the live market data session. Log in to AliceBlue again.".into()
}

/// `invalidateWsSess` (failures ignored, as the web does) then
/// `createWsSess` (must answer `status: Ok`).
pub async fn prepare_session(http: &reqwest::Client, ep: &Endpoints, jwt: &str, ucc: &str) -> Prep {
    let body = json!({"source": "API", "userId": ucc}).to_string();
    let post = |path: &'static str| {
        http.post(format!("{}{}", ep.base, path))
            .header("Authorization", format!("Bearer {}", jwt))
            .header("Content-Type", "application/json")
            .timeout(PREP_TIMEOUT)
            .body(body.clone())
            .send()
    };
    match post("/open-api/od/v1/profile/invalidateWsSess").await {
        Ok(r) if r.status() == 401 || r.status() == 403 => return Prep::Refused(refused_session()),
        Ok(_) => {}
        Err(e) => tracing::debug!("AliceBlue invalidateWsSess failed: {}", e),
    }
    let resp = match post("/open-api/od/v1/profile/createWsSess").await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("AliceBlue createWsSess failed: {}", e);
            return Prep::Unavailable;
        }
    };
    if resp.status() == 401 || resp.status() == 403 {
        return Prep::Refused(refused_session());
    }
    let v: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Prep::Unavailable,
    };
    if super::status_ok(&v) {
        return Prep::Ok;
    }
    let msg = [text(v.get("emsg")), text(v.get("message"))]
        .into_iter()
        .find(|m| !m.is_empty())
        .unwrap_or_default();
    tracing::warn!(
        "AliceBlue createWsSess refused: {}",
        mapping::describe(&msg)
    );
    let lower = msg.to_ascii_lowercase();
    if lower.contains("session") || lower.contains("token") || lower.contains("unauthor") {
        Prep::Refused(refused_session())
    } else {
        Prep::Unavailable
    }
}

/// Prepare the session and open the first market socket that answers.
pub async fn open_market_socket(
    http: &reqwest::Client,
    ep: &Endpoints,
    jwt: &str,
    ucc: &str,
    connect_timeout: Duration,
) -> Open {
    match prepare_session(http, ep, jwt, ucc).await {
        Prep::Ok => {}
        Prep::Refused(m) => return Open::AuthFailed(m),
        Prep::Unavailable => return Open::Unavailable,
    }
    for url in &ep.ws {
        match tokio::time::timeout(
            connect_timeout,
            tokio_tungstenite::connect_async(url.as_str()),
        )
        .await
        {
            Ok(Ok((ws, _))) => return Open::Ready(Box::new(ws)),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(r)))
                if r.status() == 401 || r.status() == 403 =>
            {
                return Open::AuthFailed(refused_session())
            }
            Ok(Err(e)) => tracing::debug!("AliceBlue socket {} failed: {}", url, e),
            Err(_) => tracing::debug!("AliceBlue socket {} timed out", url),
        }
    }
    Open::Unavailable
}

// ---------------------------------------------------------------------------
// Live market feed
// ---------------------------------------------------------------------------

struct MarketUpstream {
    http: reqwest::Client,
    ep: Endpoints,
    jwt: crate::security::Secret,
    ucc: String,
}

#[async_trait]
impl Upstream for MarketUpstream {
    fn broker(&self) -> &'static str {
        "aliceblue"
    }

    async fn open(&self) -> Open {
        open_market_socket(
            &self.http,
            &self.ep,
            self.jwt.expose(),
            &self.ucc,
            relay::OPEN_TIMEOUT,
        )
        .await
    }

    fn session(&self) -> Box<dyn Session> {
        Box::new(MarketSession {
            login: connect_frame(self.jwt.expose(), &self.ucc),
        })
    }
}

/// Relay-side protocol: send the login, wait for its answer, forward data.
pub struct MarketSession {
    login: String,
}

impl MarketSession {
    pub fn new(jwt: &str, ucc: &str) -> Self {
        Self {
            login: connect_frame(jwt, ucc),
        }
    }
}

impl Session for MarketSession {
    fn on_open(&mut self) -> Vec<Message> {
        vec![Message::Text(self.login.clone())]
    }

    fn ready_on_open(&self) -> bool {
        false
    }

    fn on_upstream(&mut self, msg: Message) -> Step {
        let Message::Text(t) = &msg else {
            return Step::default();
        };
        let v: Value = match serde_json::from_str(t) {
            Ok(v) => v,
            Err(_) => return Step::default(),
        };
        match auth_ack(&v) {
            Some(true) => Step {
                ready: true,
                ..Default::default()
            },
            Some(false) => Step {
                auth_failed: Some(refused_session()),
                ..Default::default()
            },
            None => Step {
                down: vec![msg],
                ..Default::default()
            },
        }
    }

    fn keepalive(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Text(heartbeat_frame())))
    }
}

struct Sub {
    sub: FeedSubscription,
    snap: FeedSnap,
}

/// The live market-data feed.
pub struct AliceBlueFeed {
    upstream: Arc<MarketUpstream>,
    relay: parking_lot::Mutex<Option<RelayHandle>>,
    /// `NSE|2885` -> subscription; bounded by the manager's registry and
    /// cleaned on unsubscribe.
    subs: HashMap<String, Sub>,
}

impl AliceBlueFeed {
    pub fn new(http: reqwest::Client, ep: Endpoints, jwt: &str, ucc: &str) -> Self {
        Self {
            upstream: Arc::new(MarketUpstream {
                http,
                ep,
                jwt: crate::security::Secret::new(jwt),
                ucc: ucc.to_string(),
            }),
            relay: parking_lot::Mutex::new(None),
            subs: HashMap::new(),
        }
    }

    /// `NSE|2885` for a subscription.
    pub fn key(sub: &FeedSubscription) -> String {
        format!(
            "{}|{}",
            ab_exchange(&sub.exchange),
            feed_token(&sub.exchange, &sub.token)
        )
    }

    pub fn subscription_count(&self) -> usize {
        self.subs.len()
    }

    fn on_data(&mut self, v: &Value) -> Vec<FeedEvent> {
        let t = s(v, "t");
        if !matches!(t.as_str(), "tk" | "tf" | "dk" | "df") {
            return Vec::new();
        }
        let key = format!("{}|{}", s(v, "e"), s(v, "tk"));
        let Some(entry) = self.subs.get_mut(&key) else {
            return Vec::new();
        };
        entry.snap.apply(v);
        let snap = &entry.snap;
        let sub = &entry.sub;
        let now = now_ms();
        let mut tick = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: snap.ltp,
            last_trade_time_ms: snap.ft_ms,
            timestamp_ms: now,
            ..Default::default()
        };
        if sub.mode != FeedMode::Ltp {
            tick.open = snap.open;
            tick.high = snap.high;
            tick.low = snap.low;
            tick.close = snap.close;
            tick.volume = snap.volume;
            tick.average_price = snap.avg_price;
            tick.last_quantity = snap.ltq;
            tick.total_buy_quantity = snap.total_buy_qty;
            tick.total_sell_quantity = snap.total_sell_qty;
            tick.oi = snap.oi;
            tick.derive_change();
            if let Some(pc) = snap.change_percent {
                tick.change_percent = pc;
            }
            if let Some(cv) = snap.change {
                tick.change = round2(cv);
            }
        }
        let mut out = vec![FeedEvent::Tick(tick)];
        if sub.mode == FeedMode::Depth && matches!(t.as_str(), "dk" | "df") {
            out.push(FeedEvent::Depth(NormalizedDepth {
                symbol: sub.symbol.clone(),
                exchange: sub.exchange.clone(),
                ltp: snap.ltp,
                buy: FeedSnap::side(&snap.bids),
                sell: FeedSnap::side(&snap.asks),
                total_buy_quantity: snap.total_buy_qty,
                total_sell_quantity: snap.total_sell_qty,
                timestamp_ms: now,
            }));
        }
        out
    }
}

fn grouped(subs: &[FeedSubscription]) -> (Vec<String>, Vec<String>) {
    let mut ticks = Vec::new();
    let mut depth = Vec::new();
    for s in subs {
        let k = AliceBlueFeed::key(s);
        let list = if s.mode == FeedMode::Depth {
            &mut depth
        } else {
            &mut ticks
        };
        if !list.contains(&k) {
            list.push(k);
        }
    }
    (ticks, depth)
}

impl BrokerFeed for AliceBlueFeed {
    fn broker(&self) -> &'static str {
        "aliceblue"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let up = self.upstream.clone();
        let url = relay::ensure_started(&self.relay, move || up as Arc<dyn Upstream>)?;
        url.as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("AliceBlue feed relay address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // Every instrument is subscribed again after the relay reports
        // ready; old snapshots would mix two sessions.
        for s in self.subs.values_mut() {
            s.snap = FeedSnap::default();
        }
        Vec::new()
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        for s in subs {
            self.subs.insert(
                Self::key(s),
                Sub {
                    sub: s.clone(),
                    snap: FeedSnap::default(),
                },
            );
        }
        let (ticks, depth) = grouped(subs);
        let mut out = Vec::new();
        if !ticks.is_empty() {
            out.push(Message::Text(subscribe_frame(&ticks, false)));
        }
        if !depth.is_empty() {
            out.push(Message::Text(subscribe_frame(&depth, true)));
        }
        out
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut keys = Vec::new();
        for s in subs {
            let k = Self::key(s);
            self.subs.remove(&k);
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        if keys.is_empty() {
            Vec::new()
        } else {
            vec![Message::Text(unsubscribe_frame(&keys))]
        }
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => {
                if let Some(c) = relay::control(t) {
                    return match c {
                        Ok(()) => vec![FeedEvent::AuthOk],
                        Err(m) => vec![FeedEvent::AuthFailed(m)],
                    };
                }
                match serde_json::from_str::<Value>(t) {
                    Ok(v) => self.on_data(&v),
                    Err(_) => Vec::new(),
                }
            }
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}

// ---------------------------------------------------------------------------
// Order Status Feed
// ---------------------------------------------------------------------------

/// What `createWsToken` produced.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderToken {
    Token(String),
    /// The account cannot use the feed, or the session is gone.
    Refused(String),
    Unavailable,
}

fn order_feed_disabled() -> String {
    "AliceBlue order updates are not enabled for this account. Ask AliceBlue to enable the Order Status Feed; orders still work."
        .into()
}

/// `GET /open-api/order-notify/ws/createWsToken` -> `result[0].orderToken`
/// (an empty or non-JSON body means the feed is not enabled).
pub async fn fetch_order_token(http: &reqwest::Client, ep: &Endpoints, jwt: &str) -> OrderToken {
    let resp = match http
        .get(format!(
            "{}/open-api/order-notify/ws/createWsToken",
            ep.base
        ))
        .header("Authorization", format!("Bearer {}", jwt))
        .timeout(Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("AliceBlue createWsToken failed: {}", e);
            return OrderToken::Unavailable;
        }
    };
    let status = resp.status();
    if status == 401 || status == 403 {
        return OrderToken::Refused(
            "AliceBlue refused the order update session. Log in to AliceBlue again.".into(),
        );
    }
    if !status.is_success() {
        return OrderToken::Unavailable;
    }
    let body = resp.text().await.unwrap_or_default();
    if body.trim().is_empty() {
        tracing::warn!("AliceBlue createWsToken answered with an empty body");
        return OrderToken::Refused(order_feed_disabled());
    }
    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        tracing::warn!("AliceBlue createWsToken answered with a non-JSON body");
        return OrderToken::Refused(order_feed_disabled());
    };
    let token = v
        .get("result")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(|r| s(r, "orderToken"))
        .unwrap_or_default();
    if token.is_empty() {
        tracing::warn!(
            "AliceBlue createWsToken returned no orderToken (status {})",
            s(&v, "status")
        );
        return OrderToken::Refused(order_feed_disabled());
    }
    OrderToken::Token(token)
}

struct OrderUpstream {
    http: reqwest::Client,
    ep: Endpoints,
    jwt: crate::security::Secret,
    ucc: String,
    token: parking_lot::Mutex<Option<crate::security::Secret>>,
}

#[async_trait]
impl Upstream for OrderUpstream {
    fn broker(&self) -> &'static str {
        "aliceblue"
    }

    async fn open(&self) -> Open {
        let token = match fetch_order_token(&self.http, &self.ep, self.jwt.expose()).await {
            OrderToken::Token(t) => t,
            OrderToken::Refused(m) => return Open::AuthFailed(m),
            OrderToken::Unavailable => return Open::Unavailable,
        };
        *self.token.lock() = Some(crate::security::Secret::new(token));
        match tokio::time::timeout(
            relay::OPEN_TIMEOUT,
            tokio_tungstenite::connect_async(self.ep.order_ws.as_str()),
        )
        .await
        {
            Ok(Ok((ws, _))) => Open::Ready(Box::new(ws)),
            _ => Open::Unavailable,
        }
    }

    fn session(&self) -> Box<dyn Session> {
        let token = self
            .token
            .lock()
            .as_ref()
            .map(|t| t.expose().to_string())
            .unwrap_or_default();
        Box::new(OrderSession {
            subscribe: order_subscribe_frame(&token, &self.ucc),
            heartbeat: order_heartbeat_frame(&self.ucc),
        })
    }
}

pub fn order_subscribe_frame(order_token: &str, ucc: &str) -> String {
    json!({"orderToken": order_token, "userId": ucc}).to_string()
}

pub fn order_heartbeat_frame(ucc: &str) -> String {
    json!({"heartbeat": "h", "userId": ucc}).to_string()
}

/// Relay-side protocol of the order socket.
pub struct OrderSession {
    pub subscribe: String,
    pub heartbeat: String,
}

impl Session for OrderSession {
    fn on_open(&mut self) -> Vec<Message> {
        vec![Message::Text(self.subscribe.clone())]
    }

    fn on_upstream(&mut self, msg: Message) -> Step {
        match msg {
            m @ Message::Text(_) => Step {
                down: vec![m],
                ..Default::default()
            },
            _ => Step::default(),
        }
    }

    fn on_downstream(&mut self, _msg: Message) -> Vec<Message> {
        Vec::new()
    }

    fn keepalive(&self) -> Option<(Duration, Message)> {
        Some((ORDER_HEARTBEAT, Message::Text(self.heartbeat.clone())))
    }
}

/// The order-update feed (`FeedEvent::OrderUpdate` per `om` frame).
pub struct AliceBlueOrderFeed {
    upstream: Arc<OrderUpstream>,
    relay: parking_lot::Mutex<Option<RelayHandle>>,
    symbols: SymbolResolver,
}

impl AliceBlueOrderFeed {
    pub fn new(
        http: reqwest::Client,
        ep: Endpoints,
        jwt: &str,
        ucc: &str,
        symbols: SymbolResolver,
    ) -> Self {
        Self {
            upstream: Arc::new(OrderUpstream {
                http,
                ep,
                jwt: crate::security::Secret::new(jwt),
                ucc: ucc.to_string(),
                token: parking_lot::Mutex::new(None),
            }),
            relay: parking_lot::Mutex::new(None),
            symbols,
        }
    }
}

impl BrokerFeed for AliceBlueOrderFeed {
    fn broker(&self) -> &'static str {
        "aliceblue"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let up = self.upstream.clone();
        let url = relay::ensure_started(&self.relay, move || up as Arc<dyn Upstream>)?;
        url.as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("AliceBlue order feed relay address is invalid".into()))
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
            Message::Text(t) => {
                if let Some(c) = relay::control(t) {
                    return match c {
                        Ok(()) => vec![FeedEvent::AuthOk],
                        Err(m) => vec![FeedEvent::AuthFailed(m)],
                    };
                }
                serde_json::from_str::<Value>(t)
                    .ok()
                    .and_then(|v| mapping::order_update_from(&v, &self.symbols))
                    .map(|u| vec![FeedEvent::OrderUpdate(u)])
                    .unwrap_or_default()
            }
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }
}
