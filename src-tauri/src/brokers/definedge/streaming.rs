//! Definedge NorenWSTRTP feeds (web `streaming/definedge_websocket.py`,
//! `definedge_adapter.py`, `definedge_order_adapter.py`).
//!
//! * URL `wss://trade.definedgesecurities.com/NorenWSTRTP/`
//!   (`definedge_websocket.py:63`).
//! * connect `{"t":"c","uid","actid","source":"TRTP","susertoken"}`
//!   (`definedge_websocket.py:364-375`), ack `{"t":"ck","s":"Ok"}`
//!   (`:383-391`).
//! * touchline `{"t":"t","k":"NSE|22#BSE|508123"}`, depth `{"t":"d"}`,
//!   unsubscribe `{"t":"u"}` / `{"t":"ud"}` (`:530-625`).
//! * frames `tk`/`tf` (touchline snapshot/update), `dk`/`df` (depth),
//!   `uk`/`udk` acks (`:317-340`); keys `e, tk, lp, ft, v, o, h, l, c, pc,
//!   ap, oi, poi, toi, tbq, tsq, ltq, bp1..5/bq1..5/bo1..5,
//!   sp1..5/sq1..5/so1..5` (`definedge_adapter.py:1039-1063`); the depth
//!   feed adds `ltt` (the exchange's `HH:MM:SS` trade time). Updates carry
//!   only changed fields, so each scrip keeps a snapshot (bounded by the
//!   subscriptions, dropped on unsubscribe), and a tick names what the
//!   snapshot has not received as absent rather than sending it as 0 (web
//!   openalgo#2383).
//! * Quote rides the depth feed as well as the touchline (web
//!   `_feed_types`): a touchline acknowledgement can come without OHLC, and
//!   the touchline never carries the trade time, last quantity or the book
//!   totals, which the depth acknowledgement does.
//! * heartbeat `{"t":"h"}` every 30 s (`definedge_websocket.py:484-528`).
//! * order updates: same URL and connect, then `{"t":"o","actid"}`; frames
//!   `t == "om"` (`definedge_order_adapter.py:9-15,74-137`).

use super::mapping;
use super::DefinedgeSession;
use crate::brokers::common::streaming::{
    now_ms, AbsentFields, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
    NormalizedDepth, NormalizedTick, QuoteField, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::streaming::merge;
use crate::brokers::types::{AuthToken, DepthLevel};
use crate::error::{AppError, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Scrips per subscribe frame.
pub const BATCH: usize = 100;
pub const HEARTBEAT: Duration = Duration::from_secs(30);

/// `(uid, susertoken)` for the socket: the uid from the session's user id,
/// the token from the feed token or the stored composite.
pub fn feed_identity(auth: &AuthToken) -> Result<(String, String)> {
    let token = match auth.feed().filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => DefinedgeSession::parse(auth)?.susertoken,
    };
    let uid = auth.user_id().unwrap_or_default().trim().to_string();
    if uid.is_empty() || token.is_empty() {
        return Err(AppError::Auth(
            "Live market data needs a fresh Definedge login. Log in to Definedge again.".into(),
        ));
    }
    Ok((uid, token))
}

fn request(url: &str) -> Result<WsRequest> {
    url.into_client_request()
        .map_err(|_| AppError::Internal("Market data feed address is invalid".into()))
}

fn connect_frame(uid: &str, token: &str) -> Message {
    Message::Text(
        json!({
            "t": "c",
            "uid": uid,
            "actid": uid,
            "source": "TRTP",
            "susertoken": token,
        })
        .to_string(),
    )
}

/// `ck` -> AuthOk / AuthFailed.
fn connect_ack(m: &Map<String, Value>) -> FeedEvent {
    let s = m.get("s").and_then(Value::as_str).unwrap_or("");
    if s.eq_ignore_ascii_case("ok") {
        FeedEvent::AuthOk
    } else {
        tracing::warn!(broker = "definedge", "Feed login refused");
        FeedEvent::AuthFailed(
            "Definedge refused the live data connection. Log in to Definedge again.".into(),
        )
    }
}

fn f(v: &Value, k: &str) -> f64 {
    super::num(v, k)
}

fn i(v: &Value, k: &str) -> i64 {
    super::int(v, k)
}

/// Whether Definedge has sent `k` (web `_present_fields`: a blank or `-`
/// is not a value).
fn sent(d: &Map<String, Value>, k: &str) -> bool {
    match d.get(k) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !matches!(s.trim(), "" | "-"),
        Some(_) => true,
    }
}

/// The raw keys only the depth feed carries: a touchline update refreshes
/// a Depth view only once one of them is in the snapshot, so a touchline
/// alone never publishes an empty book (web `_DEPTH_ONLY_KEYS`).
const DEPTH_ONLY_KEYS: [&str; 8] = ["tbq", "tsq", "bp2", "sp2", "bq2", "sq2", "ltq", "ltt"];

/// A Quote is built once the snapshot holds a price, OHLC or volume.
const QUOTE_KEYS: [&str; 6] = ["lp", "o", "h", "l", "c", "v"];

/// The quote fields and the raw keys that carry them.
const QUOTE_SOURCES: [(QuoteField, &str); 10] = [
    (QuoteField::Open, "o"),
    (QuoteField::High, "h"),
    (QuoteField::Low, "l"),
    (QuoteField::Close, "c"),
    (QuoteField::Volume, "v"),
    (QuoteField::AveragePrice, "ap"),
    (QuoteField::LastQuantity, "ltq"),
    (QuoteField::TotalBuyQuantity, "tbq"),
    (QuoteField::TotalSellQuantity, "tsq"),
    (QuoteField::OpenInterest, "oi"),
];

/// What the snapshot has not received.
fn absent(d: &Map<String, Value>, trade_time: Option<i64>) -> AbsentFields {
    let mut out = AbsentFields::default();
    for (field, key) in QUOTE_SOURCES {
        if !sent(d, key) {
            out = out.with(field);
        }
    }
    if !sent(d, "pc") && !(sent(d, "c") && sent(d, "lp")) {
        out = out.with(QuoteField::Change);
    }
    if trade_time.is_none() {
        out = out.with(QuoteField::LastTradeTime);
    }
    out
}

/// The broker's trade time, epoch ms: the depth feed's `ltt` (an IST
/// `HH:MM:SS` of the day the tick came), else the feed time `ft` (epoch
/// seconds), as web `_last_trade_time`; `None` when neither was sent.
fn trade_time_ms(snapshot: &Value, received_ms: i64) -> Option<i64> {
    let clock = snapshot.get("ltt").and_then(Value::as_str).map(str::trim);
    if let Some(ms) = clock.and_then(|c| clock_on_day_of(c, received_ms)) {
        return Some(ms);
    }
    let feed_time = i(snapshot, "ft");
    (feed_time > 0).then_some(feed_time * 1000)
}

/// `HH:MM:SS` IST on the IST day of `received_ms`, or the day before when
/// that would lie more than an hour ahead of it (a trade at 23:59:59 heard
/// just after midnight).
pub(crate) fn clock_on_day_of(clock: &str, received_ms: i64) -> Option<i64> {
    let time = chrono::NaiveTime::parse_from_str(clock, "%H:%M:%S").ok()?;
    let ist = crate::clock::ist_offset();
    let day = chrono::DateTime::from_timestamp_millis(received_ms)?
        .with_timezone(&ist)
        .date_naive();
    let at = |d: chrono::NaiveDate| {
        d.and_time(time)
            .and_local_timezone(ist)
            .single()
            .map(|t| t.timestamp_millis())
    };
    let ms = at(day)?;
    if ms > received_ms + 3_600_000 {
        return day.pred_opt().and_then(at);
    }
    Some(ms)
}

/// A Definedge feed an OpenAlgo mode subscribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Feed {
    Touchline,
    Depth,
}

/// web `_feed_types`: LTP the touchline, Depth the depth feed, Quote both
/// (module doc).
fn feeds(mode: FeedMode) -> &'static [Feed] {
    match mode {
        FeedMode::Ltp => &[Feed::Touchline],
        FeedMode::Quote => &[Feed::Touchline, Feed::Depth],
        FeedMode::Depth => &[Feed::Depth],
    }
}

/// Scrip keys per feed.
#[derive(Default)]
struct ByFeed {
    touchline: Vec<String>,
    depth: Vec<String>,
}

impl ByFeed {
    fn add(&mut self, key: &str, feeds: impl IntoIterator<Item = Feed>) {
        for feed in feeds {
            match feed {
                Feed::Touchline => self.touchline.push(key.to_string()),
                Feed::Depth => self.depth.push(key.to_string()),
            }
        }
    }

    /// The touchline frames, then the depth frames.
    fn frames(&self, touchline: &str, depth: &str) -> Vec<Message> {
        let mut out = DefinedgeFeed::frames(touchline, &self.touchline);
        out.extend(DefinedgeFeed::frames(depth, &self.depth));
        out
    }
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

impl SubInfo {
    fn of(s: &FeedSubscription) -> Self {
        Self {
            symbol: s.symbol.clone(),
            exchange: s.exchange.clone(),
            mode: s.mode,
        }
    }
}

/// Market-data feed.
pub struct DefinedgeFeed {
    url: String,
    uid: String,
    token: crate::security::Secret,
    subs: HashMap<String, SubInfo>,
    cache: HashMap<String, Map<String, Value>>,
}

/// `EXCH|token` scrip key (index exchanges on their cash exchange).
pub fn scrip(s: &FeedSubscription) -> String {
    let exch = if s.brexchange.is_empty() {
        super::data::api_exchange(&s.exchange).to_string()
    } else {
        s.brexchange.clone()
    };
    format!("{}|{}", exch, s.token)
}

impl DefinedgeFeed {
    pub fn new(url: &str, uid: &str, token: &str) -> Self {
        Self {
            url: url.to_string(),
            uid: uid.to_string(),
            token: crate::security::Secret::new(token),
            subs: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    /// Cached scrip snapshots (bounded by the subscriptions).
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    fn frames(t: &str, keys: &[String]) -> Vec<Message> {
        keys.chunks(BATCH)
            .map(|c| Message::Text(json!({"t": t, "k": c.join("#")}).to_string()))
            .collect()
    }

    /// The tick for a subscriber at `sub.mode`, from what the snapshot
    /// holds; `None` when that mode has nothing to send yet (LTP without a
    /// price, a Quote before any price, OHLC or volume; web
    /// `_normalize_raw_data`).
    fn tick(sub: &SubInfo, d: &Map<String, Value>) -> Option<NormalizedTick> {
        let ready = match sub.mode {
            FeedMode::Ltp => sent(d, "lp"),
            FeedMode::Quote | FeedMode::Depth => QUOTE_KEYS.iter().any(|k| d.contains_key(*k)),
        };
        if !ready {
            return None;
        }
        let v = Value::Object(d.clone());
        let received = now_ms();
        let trade_time = trade_time_ms(&v, received);
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: f(&v, "lp"),
            open: f(&v, "o"),
            high: f(&v, "h"),
            low: f(&v, "l"),
            close: f(&v, "c"),
            volume: i(&v, "v"),
            average_price: f(&v, "ap"),
            last_quantity: i(&v, "ltq"),
            total_buy_quantity: i(&v, "tbq"),
            total_sell_quantity: i(&v, "tsq"),
            oi: i(&v, "oi"),
            change: 0.0,
            change_percent: f(&v, "pc"),
            last_trade_time_ms: trade_time.unwrap_or(0),
            timestamp_ms: received,
            absent: absent(d, trade_time),
            ..Default::default()
        };
        let pc = t.change_percent;
        t.derive_change();
        if t.change == 0.0 && pc != 0.0 {
            t.change_percent = pc;
        }
        Some(t)
    }

    fn depth(sub: &SubInfo, d: &Map<String, Value>) -> NormalizedDepth {
        let v = Value::Object(d.clone());
        let lvl = |p: &str, q: &str, o: &str, n: usize| DepthLevel {
            price: f(&v, &format!("{}{}", p, n)),
            quantity: i(&v, &format!("{}{}", q, n)),
            orders: i(&v, &format!("{}{}", o, n)),
        };
        NormalizedDepth {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            ltp: f(&v, "lp"),
            buy: (1..=5).map(|n| lvl("bp", "bq", "bo", n)).collect(),
            sell: (1..=5).map(|n| lvl("sp", "sq", "so", n)).collect(),
            total_buy_quantity: i(&v, "tbq"),
            total_sell_quantity: i(&v, "tsq"),
            timestamp_ms: now_ms(),
            ..Default::default()
        }
    }

    /// Decode one text frame.
    pub fn parse_text(&mut self, frame: &str) -> Vec<FeedEvent> {
        let Ok(Value::Object(m)) = serde_json::from_str::<Value>(frame) else {
            return Vec::new();
        };
        let t = m.get("t").and_then(Value::as_str).unwrap_or("").to_string();
        match t.as_str() {
            "ck" => vec![connect_ack(&m)],
            "h" | "hk" | "uk" | "udk" => vec![FeedEvent::Heartbeat],
            "tk" | "tf" | "dk" | "df" => {
                let key = format!(
                    "{}|{}",
                    m.get("e").and_then(Value::as_str).unwrap_or(""),
                    super::text(&Value::Object(m.clone()), "tk")
                );
                let Some(sub) = self.subs.get(&key).cloned() else {
                    return Vec::new();
                };
                let entry = self.cache.entry(key).or_default();
                merge(entry, &m);
                let snapshot = entry.clone();
                // A depth-feed message is depth even with level 1 only; a
                // touchline refreshes Depth once the snapshot holds a book.
                let has_book = t == "dk"
                    || t == "df"
                    || DEPTH_ONLY_KEYS.iter().any(|k| snapshot.contains_key(*k));
                let mut out: Vec<FeedEvent> = Self::tick(&sub, &snapshot)
                    .map(FeedEvent::Tick)
                    .into_iter()
                    .collect();
                if sub.mode == FeedMode::Depth && has_book {
                    out.push(FeedEvent::Depth(Self::depth(&sub, &snapshot)));
                }
                out
            }
            _ => Vec::new(),
        }
    }
}

impl BrokerFeed for DefinedgeFeed {
    fn broker(&self) -> &'static str {
        "definedge"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url)
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.cache.clear();
        vec![connect_frame(&self.uid, self.token.expose())]
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut keys = ByFeed::default();
        for s in subs {
            let k = scrip(s);
            keys.add(&k, feeds(s.mode).iter().copied());
            self.subs.insert(k, SubInfo::of(s));
        }
        keys.frames("t", "d")
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut keys = ByFeed::default();
        for s in subs {
            let k = scrip(s);
            keys.add(&k, feeds(s.mode).iter().copied());
            self.subs.remove(&k);
            self.cache.remove(&k);
        }
        keys.frames("u", "ud")
    }

    /// Only the feeds that differ: Quote to Depth drops the touchline and
    /// keeps the depth feed (and the snapshot) it already has.
    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        let k = scrip(new);
        let (was, now) = (feeds(old.mode), feeds(new.mode));
        let mut gained = ByFeed::default();
        gained.add(&k, now.iter().copied().filter(|f| !was.contains(f)));
        let mut lost = ByFeed::default();
        lost.add(&k, was.iter().copied().filter(|f| !now.contains(f)));
        self.subs.insert(k, SubInfo::of(new));
        let mut out = gained.frames("t", "d");
        out.extend(lost.frames("u", "ud"));
        out
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_text(t),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Text(r#"{"t":"h"}"#.to_string())))
    }
}

/// Order-update feed: the connect frame and the order subscription
/// `{"t":"o","actid"}` go out together (Noren processes frames in order;
/// the shared manager sends no subscribe frames on a feed without
/// instruments), so no reply channel is needed.
pub struct DefinedgeOrderFeed {
    url: String,
    uid: String,
    token: crate::security::Secret,
    symbols: SymbolResolver,
}

impl DefinedgeOrderFeed {
    pub fn new(url: &str, uid: &str, token: &str, symbols: SymbolResolver) -> Self {
        Self {
            url: url.to_string(),
            uid: uid.to_string(),
            token: crate::security::Secret::new(token),
            symbols,
        }
    }

    pub fn parse_text(&self, frame: &str) -> Vec<FeedEvent> {
        let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(frame) else {
            return Vec::new();
        };
        match v.get("t").and_then(Value::as_str).unwrap_or("") {
            "ck" => match &v {
                Value::Object(m) => vec![connect_ack(m)],
                _ => Vec::new(),
            },
            "om" => vec![FeedEvent::OrderUpdate(mapping::order_update(
                &v,
                &self.symbols,
            ))],
            "h" | "hk" | "ok" | "uok" => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }
}

impl BrokerFeed for DefinedgeOrderFeed {
    fn broker(&self) -> &'static str {
        "definedge"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url)
    }

    fn on_connected(&mut self) -> Vec<Message> {
        vec![
            connect_frame(&self.uid, self.token.expose()),
            Message::Text(json!({"t": "o", "actid": self.uid}).to_string()),
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
            Message::Text(t) => self.parse_text(t),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Text(r#"{"t":"h"}"#.to_string())))
    }
}
