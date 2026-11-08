//! 5paisa market feed (web `streaming/fivepaisa_websocket.py`,
//! `fivepaisa_adapter.py`, `fivepaisa_mapping.py`).
//!
//! * Host from the access token's `RedirectServer` claim: A
//!   `wss://aopenfeed.5paisa.com/feeds/api/chat`, B `wss://bopenfeed…`,
//!   C `wss://openfeed…`, otherwise `wss://openfeed.5paisa.com/Feeds/api/chat`.
//!   Connect with `?Value1=<access_token>|<client_code>`; no login frame.
//! * Requests are JSON text:
//!   `{"Method":"MarketFeedV3"|"MarketDepthService","Operation":"Subscribe"|
//!   "Unsubscribe","ClientCode":..,"MarketFeedData":[{"Exch","ExchType",
//!   "ScripCode"}]}`, up to 50 scrips per frame. LTP and Quote ride
//!   `MarketFeedV3`; Depth subscribes `MarketDepthService` and also
//!   `MarketFeedV3`, because depth frames carry no traded price.
//! * Frames are a JSON object or an array of objects, each naming its
//!   instrument by `Exch`, `ExchType` and `Token` together (one token can
//!   be two instruments). Routed by kind (web openalgo#2396): MarketFeedV3
//!   frames (`LastRate, OpenRate, High, Low, PClose, TotalQty, LastQty,
//!   AvgRate, TBidQ, TOffQ, BidRate, OffRate, TickDt` as `/Date(ms)/`,
//!   `ChgPcnt`) make the quote tick; MarketDepthService frames (`TBidQ,
//!   TOffQ, Details[{BbBuySellFlag 66|83, Price, Quantity,
//!   NumberOfOrders}]`) the book, which the bridge joins to the last quote;
//!   any other frame (OI) makes nothing.
//! * A zero `LastRate/OpenRate/High/Low/PClose/BidRate/OffRate/AvgRate`
//!   keeps the last non-zero value seen for the instrument (snapshot merge),
//!   kept only while it is subscribed.
//! * WebSocket ping every 10 s.
//!
//! The order-confirmation stream is not used: 5paisa allows one feed
//! connection per token and drops the other, so the web polls REST instead.

use super::mapping::{ms_date_epoch, num, text};
use crate::brokers::common::streaming::{
    now_ms, AbsentFields, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
    NormalizedDepth, NormalizedTick, QuoteField, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::secret::Secret;
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const MARKET_FEED: &str = "MarketFeedV3";
pub const MARKET_DEPTH: &str = "MarketDepthService";
/// web: up to 50 scrips per frame.
pub const FRAME_BATCH: usize = 50;
const SNAPSHOT_FIELDS: &[&str] = &[
    "LastRate", "OpenRate", "High", "Low", "PClose", "BidRate", "OffRate", "AvgRate",
];

/// `RedirectServer` claim of the access-token JWT, `default` when absent.
pub fn redirect_server(token: &str) -> String {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return "default".into();
    }
    let payload = parts[1].trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| {
            v.get("RedirectServer")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "default".into())
}

/// Feed host for a `RedirectServer` value.
pub fn feed_url(server: &str) -> &'static str {
    match server {
        "A" => "wss://aopenfeed.5paisa.com/feeds/api/chat",
        "B" => "wss://bopenfeed.5paisa.com/feeds/api/chat",
        "C" => "wss://openfeed.5paisa.com/feeds/api/chat",
        _ => "wss://openfeed.5paisa.com/Feeds/api/chat",
    }
}

/// Feed `Exch` / `ExchType` from the broker exchange (web
/// `FivePaisaExchangeMapper`, defaults N / C).
pub fn feed_codes(brexchange: &str) -> (&'static str, &'static str) {
    let e = match brexchange {
        "BSE" | "BFO" | "BSE_INDEX" | "BCD" => "B",
        "MCX" => "M",
        _ => "N",
    };
    let t = match brexchange {
        "NFO" | "BFO" | "MCX" => "D",
        "CDS" | "BCD" => "U",
        _ => "C",
    };
    (e, t)
}

/// Methods a mode needs.
pub fn methods(mode: FeedMode) -> &'static [&'static str] {
    match mode {
        FeedMode::Depth => &[MARKET_DEPTH, MARKET_FEED],
        _ => &[MARKET_FEED],
    }
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
    scrip: Value,
}

/// One instrument on the feed. The scrip code alone is not unique: NSE
/// cash and derivatives, or NSE and BSE, can share one, and each is another
/// instrument (web openalgo#2396 and #2397, `_scrip_key`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ScripKey {
    exch: String,
    exch_type: String,
    token: String,
}

impl ScripKey {
    fn new(exch: &str, exch_type: &str, token: &str) -> Self {
        Self {
            exch: exch.trim().to_ascii_uppercase(),
            exch_type: exch_type.trim().to_ascii_uppercase(),
            token: token.trim().to_string(),
        }
    }

    fn of_subscription(s: &FeedSubscription) -> Self {
        let (exch, exch_type) = feed_codes(&s.brexchange);
        Self::new(exch, exch_type, &s.token)
    }

    /// A frame's `Exch`, `ExchType` and `Token`; none without a token.
    fn of_frame(frame: &Value) -> Option<Self> {
        let token = text(frame, "Token");
        (!token.is_empty())
            .then(|| Self::new(&text(frame, "Exch"), &text(frame, "ExchType"), &token))
    }
}

pub struct FivepaisaFeed {
    /// Carries the access token in its query.
    url: Secret,
    client_code: String,
    subs: HashMap<ScripKey, SubInfo>,
    /// The held prices of each subscribed instrument's last MarketFeedV3
    /// frame; removed with the subscription and on every new connection.
    snapshots: HashMap<ScripKey, Map<String, Value>>,
}

fn scrip(s: &FeedSubscription) -> Value {
    let (e, t) = feed_codes(&s.brexchange);
    let code = s
        .token
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| json!(s.token));
    json!({"Exch": e, "ExchType": t, "ScripCode": code})
}

/// What a 5paisa frame is (web openalgo#2396, `_on_data`): a
/// MarketDepthService frame (`Details`) feeds Depth subscribers, a
/// MarketFeedV3 frame (`LastRate`) the quote; anything else (an OI frame)
/// is published on its own to nobody.
enum FrameKind {
    Depth,
    Feed,
    Other,
}

fn frame_kind(frame: &Value) -> FrameKind {
    if frame.get("Details").is_some() {
        FrameKind::Depth
    } else if frame.get("LastRate").is_some() {
        FrameKind::Feed
    } else {
        FrameKind::Other
    }
}

/// The tick a MarketFeedV3 frame (its prices already held) makes for `sub`.
/// 5paisa's own `ChgPcnt` is the percent change when it is sent. OI is not
/// on this stream, so it is left out, and so is the trade time when
/// `TickDt` is not one: the feed sends `/Date(-62135596800000)/`
/// (DateTime.MinValue) when it has none, and only a positive epoch is a
/// time (web openalgo#2396, `_parse_fivepaisa_time`), never the receive
/// time.
fn quote_tick(sub: &SubInfo, m: &Value) -> NormalizedTick {
    let mut t = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode: sub.mode.code(),
        ltp: num(m, "LastRate"),
        last_trade_time_ms: ms_date_epoch(&text(m, "TickDt")),
        timestamp_ms: now_ms(),
        absent: AbsentFields::of(&[QuoteField::OpenInterest]),
        ..Default::default()
    };
    if t.last_trade_time_ms <= 0 {
        t.absent = t.absent.with(QuoteField::LastTradeTime);
    }
    if sub.mode != FeedMode::Ltp {
        t.open = num(m, "OpenRate");
        t.high = num(m, "High");
        t.low = num(m, "Low");
        t.close = num(m, "PClose");
        t.volume = num(m, "TotalQty") as i64;
        t.last_quantity = num(m, "LastQty") as i64;
        t.average_price = num(m, "AvgRate");
        t.total_buy_quantity = num(m, "TBidQ") as i64;
        t.total_sell_quantity = num(m, "TOffQ") as i64;
        t.derive_change();
        if m.get("ChgPcnt").is_some_and(|v| !v.is_null()) {
            t.change_percent = num(m, "ChgPcnt");
        }
    }
    t
}

/// The five-level book of a MarketDepthService frame (`BbBuySellFlag` 66
/// buy, 83 sell) with its own totals; `ltp` is the held quote price.
fn depth_book(sub: &SubInfo, frame: &Value, ltp: f64) -> NormalizedDepth {
    let details = frame
        .get("Details")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let side = |flag: i64| {
        let mut v: Vec<DepthLevel> = details
            .iter()
            .filter(|d| num(d, "BbBuySellFlag") as i64 == flag)
            .map(|d| DepthLevel {
                price: num(d, "Price"),
                quantity: num(d, "Quantity") as i64,
                orders: num(d, "NumberOfOrders") as i64,
            })
            .take(5)
            .collect();
        v.resize(5, DepthLevel::default());
        v
    };
    NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        ltp,
        buy: side(66),
        sell: side(83),
        total_buy_quantity: num(frame, "TBidQ") as i64,
        total_sell_quantity: num(frame, "TOffQ") as i64,
        timestamp_ms: now_ms(),
    }
}

impl FivepaisaFeed {
    pub fn new(host: &str, access_token: &str, client_code: &str) -> Self {
        Self {
            url: Secret::new(format!("{}?Value1={}|{}", host, access_token, client_code)),
            client_code: client_code.to_string(),
            subs: HashMap::new(),
            snapshots: HashMap::new(),
        }
    }

    pub fn cached(&self) -> usize {
        self.snapshots.len()
    }

    fn frames(&self, op: &str, by_method: BTreeMap<&'static str, Vec<Value>>) -> Vec<Message> {
        let mut out = Vec::new();
        for (method, scrips) in by_method {
            for chunk in scrips.chunks(FRAME_BATCH) {
                out.push(Message::Text(
                    json!({
                        "Method": method,
                        "Operation": op,
                        "ClientCode": self.client_code,
                        "MarketFeedData": chunk,
                    })
                    .to_string(),
                ));
            }
        }
        out
    }

    /// A MarketFeedV3 frame with each zero price replaced by the last
    /// non-zero one seen for the instrument; quantities follow the frame.
    fn hold_prices(&mut self, key: &ScripKey, frame: &Value) -> Value {
        let snap = self.snapshots.entry(key.clone()).or_default();
        let mut merged = frame.as_object().cloned().unwrap_or_default();
        for f in SNAPSHOT_FIELDS {
            let v = frame.get(*f).map(num_of).unwrap_or(0.0);
            if v == 0.0 {
                if let Some(prev) = snap.get(*f) {
                    merged.insert((*f).to_string(), prev.clone());
                }
            } else {
                snap.insert((*f).to_string(), json!(v));
            }
        }
        Value::Object(merged)
    }

    /// The events of one frame, for the subscription of its own exchange,
    /// segment and token only. Nothing is kept for an instrument no one
    /// subscribes (web openalgo#2397): a late frame after an unsubscribe
    /// must not bring its snapshot back.
    fn item(&mut self, frame: &Value) -> Vec<FeedEvent> {
        let Some(key) = ScripKey::of_frame(frame) else {
            return Vec::new();
        };
        let Some(sub) = self.subs.get(&key).cloned() else {
            return Vec::new();
        };
        match frame_kind(frame) {
            FrameKind::Depth if sub.mode == FeedMode::Depth => {
                let ltp = self
                    .snapshots
                    .get(&key)
                    .and_then(|s| s.get("LastRate"))
                    .map(num_of)
                    .unwrap_or(0.0);
                vec![FeedEvent::Depth(depth_book(&sub, frame, ltp))]
            }
            FrameKind::Feed => {
                let m = self.hold_prices(&key, frame);
                vec![FeedEvent::Tick(quote_tick(&sub, &m))]
            }
            FrameKind::Depth | FrameKind::Other => Vec::new(),
        }
    }

    /// Decode one text frame.
    pub fn parse_text(&mut self, frame: &str) -> Vec<FeedEvent> {
        match serde_json::from_str::<Value>(frame) {
            Ok(v @ Value::Object(_)) => self.item(&v),
            Ok(Value::Array(a)) => a
                .iter()
                .filter(|v| v.is_object())
                .flat_map(|v| self.item(v))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn register(&mut self, s: &FeedSubscription) -> Value {
        let sc = scrip(s);
        self.subs.insert(
            ScripKey::of_subscription(s),
            SubInfo {
                symbol: s.symbol.clone(),
                exchange: s.exchange.clone(),
                mode: s.mode,
                scrip: sc.clone(),
            },
        );
        sc
    }
}

fn num_of(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

impl BrokerFeed for FivepaisaFeed {
    fn broker(&self) -> &'static str {
        "fivepaisa"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .expose()
            .into_client_request()
            .map_err(|_| AppError::Internal("5paisa feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.snapshots.clear();
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut by: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
        for s in subs {
            let sc = self.register(s);
            for m in methods(s.mode) {
                by.entry(m).or_default().push(sc.clone());
            }
        }
        self.frames("Subscribe", by)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut by: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
        for s in subs {
            let key = ScripKey::of_subscription(s);
            let sc = self
                .subs
                .remove(&key)
                .map(|i| i.scrip)
                .unwrap_or_else(|| scrip(s));
            self.snapshots.remove(&key);
            for m in methods(s.mode) {
                by.entry(m).or_default().push(sc.clone());
            }
        }
        self.frames("Unsubscribe", by)
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        let sc = self.register(new);
        let (was, now) = (methods(old.mode), methods(new.mode));
        let mut drop: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
        let mut add: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
        for m in was.iter().filter(|m| !now.contains(m)) {
            drop.entry(m).or_default().push(sc.clone());
        }
        for m in now.iter().filter(|m| !was.contains(m)) {
            add.entry(m).or_default().push(sc.clone());
        }
        let mut out = self.frames("Unsubscribe", drop);
        out.extend(self.frames("Subscribe", add));
        out
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_text(t),
            Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((Duration::from_secs(10), Message::Ping(Vec::new())))
    }
}
