//! Nuvama's market and order feeds (web `streaming/nuvama_adapter.py`,
//! `nuvama_socket.py`, `nuvama_order_adapter.py`), each reached through
//! the loopback line relay (`tcp_relay`).
//!
//! Requests are the SDK's (`feed/feed.py`, `feed/orders_feed.py`):
//! `{"request": {"streaming_type", "data": {"accType", "symbols": [{"symbol":
//! token}]}, "formFactor": "P", "appID", "response_format": "json",
//! "request_type": "subscribe" | "unsubscribe"}, "echo": {}}`. A subscribe
//! replaces its streaming type's whole symbol set (checked live by the web),
//! so the feed keeps each set and sends it whole on every change; an empty
//! set goes as an unsubscribe of the type. Modes 1 and 2 ride the reduced
//! quote stream (`quote`); Depth adds the depth stream (`quote2`), whose
//! packets carry only the five levels each side. Every answer line is
//! `{"response": {"streaming_type", "data": {..}}}`.

use super::mapping::{self, number, text};
use super::session::NuvamaSession;
use super::tcp_relay::{self, Endpoint, LineRelay};
use super::NuvamaUrls;
use crate::brokers::common::relay;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, QuoteField, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::Secret;
use chrono::NaiveDateTime;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const QUOTE: &str = "quote";
pub const DEPTH: &str = "quote2";
pub const ORDERS: &str = "orderFiler";
/// web `STALL_SECONDS`: with symbols subscribed and no data for this long,
/// reconnect.
pub const STALL: Duration = Duration::from_secs(180);
const IST_OFFSET_MS: i64 = 19_800_000;

/// Where a feed's relay side is: its own relay, started on first use, or a
/// socket that plays the relay (the feed corpus's fake broker sends the
/// ready frame and Nuvama's lines and takes the requests), so nothing
/// connects to Nuvama.
enum RelaySide {
    Own(Mutex<Option<LineRelay>>, Endpoint),
    At(String),
}

impl RelaySide {
    fn of(urls: &NuvamaUrls) -> Self {
        RelaySide::Own(
            Mutex::new(None),
            Endpoint {
                host: urls.stream_host.clone(),
                port: urls.stream_port,
            },
        )
    }

    fn request(&self, what: &str) -> Result<WsRequest> {
        let url = match self {
            RelaySide::Own(slot, endpoint) => tcp_relay::ensure_started(slot, endpoint)?,
            RelaySide::At(url) => url.clone(),
        };
        url.as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal(format!("Nuvama {} relay address is invalid", what)))
    }
}

/// One request line (web `NuvamaStreamClient._request`).
pub fn request(streaming_type: &str, data: Value, app_id: &str, request_type: &str) -> Message {
    Message::Text(
        json!({
            "request": {
                "streaming_type": streaming_type,
                "data": data,
                "formFactor": "P",
                "appID": app_id,
                "response_format": "json",
                "request_type": request_type,
            },
            "echo": {},
        })
        .to_string(),
    )
}

/// `{"response": {"streaming_type", "data"}}` -> its type and data.
pub fn response(text: &str) -> Option<(String, Value)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let r = v.get("response")?;
    let kind = r
        .get("streaming_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let data = r.get("data").cloned().unwrap_or(Value::Null);
    Some((kind.to_string(), data))
}

fn relay_events(msg: &Message) -> Option<Vec<FeedEvent>> {
    match msg {
        Message::Text(t) => relay::control(t).map(|c| match c {
            Ok(()) => vec![FeedEvent::AuthOk],
            Err(m) => vec![FeedEvent::AuthFailed(m)],
        }),
        Message::Ping(_) | Message::Pong(_) => Some(vec![FeedEvent::Heartbeat]),
        _ => None,
    }
}

/// `21 Jul 2023, 12:44:55 PM` or `21/07/2023 12:35:39` (IST) -> epoch ms
/// (web `_epoch_ms`); `None` when it does not read.
pub fn trade_time_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    ["%d %b %Y, %I:%M:%S %p", "%d/%m/%Y %H:%M:%S"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .map(|t| t.and_utc().timestamp_millis() - IST_OFFSET_MS)
}

/// One `quote` packet as a tick at `sub`'s mode (web `_normalize`). A
/// field the packet leaves out is left out of the frame (the web sends it
/// as 0), and a trade time that does not read is left out (the web sends
/// the receipt time).
pub fn quote_tick(sub: &FeedSubscription, q: &Value) -> NormalizedTick {
    let mut t = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode: sub.mode.code(),
        timestamp_ms: now_ms(),
        ..Default::default()
    };
    match number(q.get("ltp")) {
        Some(v) => t.ltp = v,
        None => t.mark_absent(QuoteField::Ltp),
    }
    // The previous close, with the change, from the LTP frame up when the
    // packet carries one (web `if close:`).
    match number(q.get("c")).filter(|c| *c != 0.0) {
        Some(c) => {
            t.close = c;
            t.mark_carried(QuoteField::Close);
        }
        None => t.mark_absent(QuoteField::Close),
    }
    let ltt = text(q.get("altt"));
    let ltt = if ltt.is_empty() {
        text(q.get("ltt"))
    } else {
        ltt
    };
    match trade_time_ms(&ltt) {
        Some(ms) => t.last_trade_time_ms = ms,
        None => t.mark_absent(QuoteField::LastTradeTime),
    }
    if sub.mode != FeedMode::Ltp {
        quote_fields(&mut t, q);
    }
    // The circuits go out in Depth frames only (the dialect), but a quote
    // that comes while the instrument is held at Quote still serves the
    // Depth frame a later book builds on it (web: the book is normalised
    // with the last quote at mode 3).
    t.extras.upper_circuit = number(q.get("hCL"));
    t.extras.lower_circuit = number(q.get("lCL"));
    t.derive_change();
    t
}

/// A packet key, the quote field it fills and where on the tick it goes.
type Slot<T> = (&'static str, QuoteField, fn(&mut NormalizedTick) -> &mut T);

/// The Quote fields of a packet, each only when the packet carries it.
fn quote_fields(t: &mut NormalizedTick, q: &Value) {
    use QuoteField as F;
    let prices: [Slot<f64>; 4] = [
        ("o", F::Open, |t| &mut t.open),
        ("h", F::High, |t| &mut t.high),
        ("l", F::Low, |t| &mut t.low),
        ("avgPrc", F::AveragePrice, |t| &mut t.average_price),
    ];
    for (key, field, slot) in prices {
        match number(q.get(key)) {
            Some(v) => *slot(t) = v,
            None => t.mark_absent(field),
        }
    }
    let counts: [Slot<i64>; 5] = [
        ("vol", F::Volume, |t| &mut t.volume),
        ("ltq", F::LastQuantity, |t| &mut t.last_quantity),
        ("tBQ", F::TotalBuyQuantity, |t| &mut t.total_buy_quantity),
        ("tSQ", F::TotalSellQuantity, |t| &mut t.total_sell_quantity),
        ("oI", F::OpenInterest, |t| &mut t.oi),
    ];
    for (key, field, slot) in counts {
        match number(q.get(key)) {
            Some(v) => *slot(t) = v.trunc() as i64,
            None => t.mark_absent(field),
        }
    }
    // The web sends OI in its Quote frames too.
    if number(q.get("oI")).is_some() {
        t.mark_carried(F::OpenInterest);
    }
}

fn depth_side(rows: Option<&Value>) -> Vec<DepthLevel> {
    rows.and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .take(5)
                .map(|l| DepthLevel {
                    price: mapping::num(l.get("price")),
                    quantity: mapping::int(l.get("qty")),
                    orders: mapping::int(l.get("no")),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One `quote2` packet as a book (web `_depth`, which pads each side to
/// five zero levels; the desktop never pads). It carries no totals and no
/// price of its own: the frame takes the tick's.
pub fn depth_book(sub: &FeedSubscription, d: &Value) -> NormalizedDepth {
    use QuoteField as F;
    let mut book = NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        buy: depth_side(d.get("bidValues")),
        sell: depth_side(d.get("askValues")),
        timestamp_ms: now_ms(),
        ..Default::default()
    };
    book.absent = book
        .absent
        .with(F::Ltp)
        .with(F::TotalBuyQuantity)
        .with(F::TotalSellQuantity);
    book
}

/// The market data `BrokerFeed`.
pub struct NuvamaFeed {
    relay: RelaySide,
    app_id: Secret,
    account_type: String,
    /// Token -> subscription at its effective mode.
    subs: HashMap<String, FeedSubscription>,
    /// The sets this connection last sent, per streaming type.
    sent: HashMap<&'static str, BTreeSet<String>>,
    last_data: Instant,
}

impl NuvamaFeed {
    pub fn new(urls: &NuvamaUrls, session: &NuvamaSession, _symbols: SymbolResolver) -> Self {
        Self {
            relay: RelaySide::of(urls),
            app_id: session.app_id_key.clone(),
            account_type: session.stream_account_type().to_string(),
            subs: HashMap::new(),
            sent: HashMap::new(),
            last_data: Instant::now(),
        }
    }

    /// The feed with its relay side at `url` (the feed corpus).
    pub fn with_relay_url(url: &str, session: &NuvamaSession) -> Self {
        let mut feed = Self::new(&NuvamaUrls::default(), session, SymbolResolver::new());
        feed.relay = RelaySide::At(url.to_string());
        feed
    }

    fn wanted(&self, kind: &str) -> BTreeSet<String> {
        self.subs
            .iter()
            .filter(|(_, s)| kind == QUOTE || s.mode == FeedMode::Depth)
            .map(|(t, _)| t.clone())
            .collect()
    }

    /// The requests that bring each streaming type to the wanted set.
    fn sync(&mut self) -> Vec<Message> {
        let mut frames = Vec::new();
        for kind in [QUOTE, DEPTH] {
            let want = self.wanted(kind);
            let had = self.sent.get(kind).cloned().unwrap_or_default();
            if want == had {
                continue;
            }
            let symbols: Vec<Value> = want.iter().map(|s| json!({ "symbol": s })).collect();
            let request_type = if want.is_empty() {
                "unsubscribe"
            } else {
                "subscribe"
            };
            let data = json!({ "accType": self.account_type, "symbols": symbols });
            frames.push(request(kind, data, self.app_id.expose(), request_type));
            self.sent.insert(kind, want);
        }
        // Data starts after a subscribe; the wait before it is no stall.
        self.last_data = Instant::now();
        frames
    }

    fn on_packet(&mut self, kind: &str, data: &Value) -> Vec<FeedEvent> {
        let token = {
            let t = text(data.get("sym"));
            if t.is_empty() {
                text(data.get("symbol"))
            } else {
                t
            }
        };
        let Some(sub) = self.subs.get(&token) else {
            return Vec::new();
        };
        match kind {
            QUOTE => vec![FeedEvent::Tick(quote_tick(sub, data))],
            DEPTH if sub.mode == FeedMode::Depth => vec![FeedEvent::Depth(depth_book(sub, data))],
            _ => Vec::new(),
        }
    }
}

impl BrokerFeed for NuvamaFeed {
    fn broker(&self) -> &'static str {
        "nuvama"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.relay.request("feed")
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // A new socket has nothing subscribed.
        self.subs.clear();
        self.sent.clear();
        self.last_data = Instant::now();
        Vec::new()
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    /// The relay opens the TCP socket before its ready frame comes.
    fn auth_ack_deadline(&self) -> Option<Duration> {
        Some(relay::READY_DEADLINE)
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        for s in subs {
            self.subs.insert(s.token.clone(), s.clone());
        }
        self.sync()
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        for s in subs {
            self.subs.remove(&s.token);
        }
        self.sync()
    }

    /// One instrument at another mode changes at most the depth set.
    fn mode_change_frames(
        &mut self,
        _old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        self.subs.insert(new.token.clone(), new.clone());
        self.sync()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        if let Some(ev) = relay_events(msg) {
            return ev;
        }
        let Message::Text(t) = msg else {
            return Vec::new();
        };
        let Some((kind, data)) = response(t) else {
            return Vec::new();
        };
        self.last_data = Instant::now();
        self.on_packet(&kind, &data)
    }

    /// web: with symbols subscribed and nothing received for 180 s, the
    /// socket is replaced.
    fn data_stalled(&mut self) -> bool {
        let stalled = !self.subs.is_empty() && self.last_data.elapsed() > STALL;
        if stalled {
            self.last_data = Instant::now();
        }
        stalled
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}

/// One `orderFiler` packet -> an order update; `None` for anything but an
/// order packet (trade packets repeat a fill the order packet already
/// reports) (web `normalize_order_update`).
pub fn order_update(d: &Value, symbols: &SymbolResolver) -> Option<OrderUpdate> {
    if text(d.get("pTyp")) != "ORDER_UPDATE" {
        return None;
    }
    let order_id = text(d.get("oID"));
    if order_id.is_empty() {
        return None;
    }
    let exchange = text(d.get("exc")).to_ascii_uppercase();
    let status = mapping::normalize_status(&text(d.get("sts")));
    let total = mapping::int(d.get("tQty"));
    let filled = mapping::int(d.get("fQty"));
    let finished = matches!(status.as_str(), "complete" | "cancelled" | "rejected");
    let token = text(d.get("sym"));
    let tsym = text(d.get("tSym"));
    let symbol = symbols
        .by_token(&exchange, &token)
        .map(|r| r.symbol)
        .unwrap_or_else(|| symbols.oa_symbol_or_raw(&tsym, &exchange));
    Some(OrderUpdate {
        orderid: order_id,
        symbol,
        exchange,
        action: mapping::side(&text(d.get("tTyp"))),
        quantity: total,
        price: mapping::num(d.get("prc")),
        trigger_price: mapping::num(d.get("trgPrc")),
        pricetype: mapping::price_type_from_nuvama(&text(d.get("oTyp"))),
        product: mapping::product_from_nuvama(&text(d.get("pCode"))),
        filled_quantity: filled,
        pending_quantity: if finished { 0 } else { (total - filled).max(0) },
        average_price: mapping::num(d.get("avgPrc")),
        rejection_reason: if status == "rejected" {
            text(d.get("rRsn"))
        } else {
            String::new()
        },
        order_status: if status.is_empty() {
            "open".into()
        } else {
            status
        },
    })
}

/// Order updates from Nuvama's Orders Streamer: one socket of its own, the
/// subscription sent once per connection (web `standing_requests`).
pub struct NuvamaOrderFeed {
    relay: RelaySide,
    app_id: Secret,
    account_type: String,
    account_id: String,
    symbols: SymbolResolver,
}

impl NuvamaOrderFeed {
    pub fn new(urls: &NuvamaUrls, session: &NuvamaSession, symbols: SymbolResolver) -> Self {
        Self {
            relay: RelaySide::of(urls),
            app_id: session.app_id_key.clone(),
            account_type: session.stream_account_type().to_string(),
            account_id: session.account_id.clone(),
            symbols,
        }
    }

    /// The order feed with its relay side at `url` (the feed corpus).
    pub fn with_relay_url(url: &str, session: &NuvamaSession, symbols: SymbolResolver) -> Self {
        let mut feed = Self::new(&NuvamaUrls::default(), session, symbols);
        feed.relay = RelaySide::At(url.to_string());
        feed
    }
}

impl BrokerFeed for NuvamaOrderFeed {
    fn broker(&self) -> &'static str {
        "nuvama"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.relay.request("order-update")
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn auth_ack_deadline(&self) -> Option<Duration> {
        Some(relay::READY_DEADLINE)
    }

    /// The login reports no user id apart from the account, and Nuvama's
    /// packets carried that same value as their userID.
    fn on_authenticated(&mut self) -> Vec<Message> {
        let data = json!({
            "accType": self.account_type,
            "userID": self.account_id,
            "accID": self.account_id,
            "responseType": ["ORDER_UPDATE", "TRADE_UPDATE"],
        });
        vec![request(ORDERS, data, self.app_id.expose(), "subscribe")]
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
        let Message::Text(t) = msg else {
            return Vec::new();
        };
        match response(t) {
            Some((kind, data)) if kind == ORDERS => order_update(&data, &self.symbols)
                .map(FeedEvent::OrderUpdate)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }
}
