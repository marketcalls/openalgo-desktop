//! Samco broadcast feed (web `streaming/samcoWebSocket.py`,
//! `samco_adapter.py`, `samco_mapping.py`).
//!
//! * `wss://stream.samco.in`, header `x-session-token` (percent-decoded,
//!   `samcoWebSocket.py:84`).
//! * Requests are JSON terminated by a newline in the same frame
//!   (`samcoWebSocket.py:509-517`):
//!   `{"request":{"streaming_type":"quote"|"quote2","data":{"symbols":
//!   [{"symbol":"<scripCode>_<SEG>"}]},"request_type":"subscribe"|
//!   "unsubscribe","response_format":"json"}}`.
//! * Replace semantics: every subscribe frame restates the full set
//!   (`_send_subscription_state_locked`, `:467-520`). Every subscriber is on
//!   `quote` (ltp/ohlc/volume/oi); depth subscribers are also on `quote2`
//!   (bid/ask ladder). Unsubscribe sends the removed symbols on the
//!   stream(s) they were on, then the remaining full set (`:900-1001`).
//! * Frames: `quote` arrives flat with `streaming_type` beside the fields;
//!   `quote2` / `marketDepth` arrive wrapped as `{"response":{"data":{..},
//!   "streaming_type":"quote2"}}` (`_unwrap_tick`, `:522-541`). Fields of
//!   both are merged per symbol (`_normalize_market_data`, `:543-667`):
//!   `sym`/`symbol, ltp, ltq, o, h, l, c, ch, chPer, vol, oI, avgPr, bPr,
//!   bSz, aPr, aSz, tBQ/tbq, tSQ/taq, lTrdT/ltt/lttUTC, bidValues[{price,qty,
//!   no}], askValues`. The average price, the totals and the trade time are
//!   left out of a frame until Samco has sent them.
//! * Indices subscribe by `listingId` (`-23`), no suffix
//!   (`samcoWebSocket.py:849-863`); ids come from `/quote/indexQuote` and
//!   are shared with the REST adapter through [`ListingIds`].

use super::mapping::{int, num, text};
use super::ListingIds;
use crate::brokers::common::streaming::{
    now_ms, ping, round2, AbsentFields, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
    NormalizedDepth, NormalizedTick, QuoteField, WsRequest, WEB_PING_INTERVAL, WEB_PING_TIMEOUT,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// The fields only the book stream (quote2) carries.
const BOOK_FIELDS: &[&str] = &["bidValues", "askValues", "tbq", "taq"];

/// Samco's two streams on the socket: `quote` (price, OHLC, volume, OI,
/// best bid and ask) and `quote2` / `marketDepth` (the book and its
/// totals).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    Quote,
    Book,
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct SamcoFeed {
    url: String,
    token: crate::security::Secret,
    /// Streaming key (`2885_NSE`, `-23`) -> subscription. Ordered so the
    /// full-set frames are deterministic.
    subs: BTreeMap<String, SubInfo>,
    /// Merged raw fields per subscribed key; removed with the subscription.
    state: HashMap<String, Map<String, Value>>,
    listing_ids: Arc<Mutex<ListingIds>>,
}

/// Percent-decode (Python `urllib.parse.unquote`).
pub fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `{"request":{..}}` + `\n`.
pub fn request_frame(streaming_type: &str, request_type: &str, keys: &[String]) -> Message {
    let symbols: Vec<Value> = keys.iter().map(|k| json!({ "symbol": k })).collect();
    let req = json!({
        "request": {
            "streaming_type": streaming_type,
            "data": {"symbols": symbols},
            "request_type": request_type,
            "response_format": "json",
        }
    });
    Message::Text(format!("{}\n", req))
}

impl SamcoFeed {
    pub fn new(url: &str, session_token: &str, listing_ids: Arc<Mutex<ListingIds>>) -> Self {
        Self {
            url: url.to_string(),
            token: crate::security::Secret::new(unquote(session_token)),
            subs: BTreeMap::new(),
            state: HashMap::new(),
            listing_ids,
        }
    }

    /// Number of instruments with merged state (tests: proves cleanup).
    pub fn state_len(&self) -> usize {
        self.state.len()
    }

    /// Streaming key for a subscription (web `subscribe`).
    fn key(&self, s: &FeedSubscription) -> Option<String> {
        if matches!(s.exchange.as_str(), "NSE_INDEX" | "BSE_INDEX") {
            // The live feed's step puts the listing id in the token (RB-05);
            // without it, the ids a REST index quote learnt.
            if s.token.starts_with('-') {
                return Some(s.token.clone());
            }
            let id = self.listing_ids.lock().get(&s.exchange, &s.symbol);
            if id.is_none() {
                tracing::warn!(
                    "No Samco streaming id yet for index {}:{}; it streams after its first quote",
                    s.exchange,
                    s.symbol
                );
            }
            return id;
        }
        let t = s.token.trim();
        if t.is_empty() {
            return None;
        }
        if t.starts_with('-') || t.contains('_') {
            Some(t.to_string())
        } else {
            Some(format!("{}_{}", t, s.brexchange))
        }
    }

    /// The full current set, once per streaming type (`quote2` for depth
    /// subscribers, `quote` for everyone).
    fn full_set_frames(&self) -> Vec<Message> {
        let depth: Vec<String> = self
            .subs
            .iter()
            .filter(|(_, s)| s.mode == FeedMode::Depth)
            .map(|(k, _)| k.clone())
            .collect();
        let all: Vec<String> = self.subs.keys().cloned().collect();
        let mut out = Vec::new();
        if !depth.is_empty() {
            out.push(request_frame("quote2", "subscribe", &depth));
        }
        if !all.is_empty() {
            out.push(request_frame("quote", "subscribe", &all));
        }
        out
    }

    /// What one frame of `stream` makes. As the web routes them
    /// (`samco_adapter._on_data`), a quote frame is a tick for the LTP and
    /// Quote holders and a book (quote2) frame reaches Depth holders only,
    /// so a book that comes before Samco's first quote never shows a Quote
    /// holder an LTP of 0. No book is sent before Samco has sent one (the
    /// web sends a Depth holder nothing until then); after that a quote
    /// frame sends the book again with the new price (an approved
    /// difference: the web leaves it as it was until the next book).
    fn event(&mut self, stream: Stream, data: &Map<String, Value>) -> Vec<FeedEvent> {
        let symbol = {
            let k = text(data.get("symbol"));
            if k.is_empty() {
                text(data.get("sym"))
            } else {
                k
            }
        };
        let Some((key, sub)) = self.route(&symbol) else {
            return Vec::new();
        };
        let merged = self.merge(key, data);
        let now = now_ms();
        let mut out = Vec::new();
        if stream == Stream::Quote {
            out.push(FeedEvent::Tick(quote_tick(&sub, &merged, now)));
        }
        if sub.mode == FeedMode::Depth && has_book(&merged) {
            out.push(FeedEvent::Depth(book(&sub, &merged, now)));
        }
        out
    }

    /// The subscription a frame's symbol names, with its streaming key.
    /// Samco names a stock `scripCode_segment` (`2885_NSE`) and an index by
    /// its bare listing id (`-23`). A bare stock scrip code does not say its
    /// segment, and the same code on another segment is another instrument,
    /// so it is taken only when a single segment holds that code (web
    /// openalgo#2396 and #2397, `samco_adapter._on_data`).
    fn route(&self, symbol: &str) -> Option<(String, SubInfo)> {
        if let Some(sub) = self.subs.get(symbol) {
            return Some((symbol.to_string(), sub.clone()));
        }
        if symbol.is_empty() || symbol.contains('_') || symbol.starts_with('-') {
            return None;
        }
        let prefix = format!("{}_", symbol);
        let mut held = self
            .subs
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix));
        let (key, sub) = held.next()?;
        if held.next().is_some() {
            tracing::debug!(
                "Samco tick for scrip code {} matches more than one segment; dropped",
                symbol
            );
            return None;
        }
        Some((key.clone(), sub.clone()))
    }

    /// Fold `data` into what the other stream last reported for `key`
    /// (`quote` carries no ladder, `quote2` no ltp or OHLC); an empty value
    /// keeps the earlier one. The state lives as long as the subscription.
    fn merge(&mut self, key: String, data: &Map<String, Value>) -> Map<String, Value> {
        let st = self.state.entry(key).or_default();
        for (k, v) in data {
            let empty = v.is_null() || v.as_str() == Some("");
            if !empty {
                st.insert(k.clone(), v.clone());
            }
        }
        st.clone()
    }

    fn parse_text(&mut self, t: &str) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        for line in t.split('\n').map(str::trim).filter(|l| !l.is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let (stype, payload) = match v.get("response").filter(|r| r.is_object()) {
                Some(r) => (
                    text(r.get("streaming_type")),
                    r.get("data").and_then(Value::as_object).cloned(),
                ),
                None => (text(v.get("streaming_type")), v.as_object().cloned()),
            };
            let stream = match stype.as_str() {
                "quote" => Stream::Quote,
                "quote2" | "marketDepth" => Stream::Book,
                _ => continue,
            };
            if let Some(p) = payload {
                out.extend(self.event(stream, &p));
            }
        }
        out
    }
}

/// The first of `keys` Samco has reported, a 0 included: `quote2` names the
/// book totals `tbq`/`taq` and `quote` names them `tBQ`/`tSQ`, and a 0 from
/// the depth stream is a real total, not a reason to show the quote
/// stream's older one (web openalgo#2397, `_first_present`).
fn first_present<'a>(state: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| state.get(*k))
}

/// The exchange's last trade time, epoch ms (web openalgo#2396,
/// `_parse_trade_time`): `lTrdT`, then `ltt` (IST wall-clock text, "01 Oct
/// 2019, 06:47:03 PM"), then `lttUTC` (the same instant in UTC). A number is
/// epoch ms, or seconds when too small to be ms. None when Samco sent no
/// time it can read: the receive time is not the trade time.
fn trade_time_ms(state: &Map<String, Value>) -> Option<i64> {
    let ist_ms = crate::clock::IST_OFFSET_SECS * 1000;
    [("lTrdT", ist_ms), ("ltt", ist_ms), ("lttUTC", 0)]
        .into_iter()
        .find_map(|(field, offset_ms)| {
            let value = text(state.get(field));
            if value.is_empty() {
                None
            } else if is_plain_number(&value) {
                epoch_ms(&value)
            } else {
                chrono::NaiveDateTime::parse_from_str(&value, "%d %b %Y, %I:%M:%S %p")
                    .ok()
                    .map(|t| t.and_utc().timestamp_millis() - offset_ms)
            }
        })
}

/// Digits with at most one dot.
fn is_plain_number(value: &str) -> bool {
    let digits = value.replacen('.', "", 1);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// A plain number as epoch ms: seconds below 10^11, none when it is not
/// positive.
fn epoch_ms(value: &str) -> Option<i64> {
    let n = value.parse::<f64>().ok()?.trunc();
    if n <= 0.0 || n >= i64::MAX as f64 {
        return None;
    }
    if n >= 1e11 {
        Some(n as i64)
    } else {
        (n as i64).checked_mul(1000)
    }
}

/// The tick for `sub` from the merged fields: a quote packet, whatever
/// the subscription, with Samco's OI when it sent one (the web reads `oI`
/// and then drops it from every mode: lost data, kept here). Every field
/// Samco has not sent yet (the price, OHLC, volume, last quantity, average
/// price, book totals, trade time) is marked absent, so a client is not
/// shown an invented 0 or the receive time (web openalgo#2396; the web
/// still sends 0 for some).
fn quote_tick(sub: &SubInfo, merged: &Map<String, Value>, ts: i64) -> NormalizedTick {
    let g = |k: &str| merged.get(k);
    let tbq = first_present(merged, &["tbq", "tBQ"]);
    let tsq = first_present(merged, &["taq", "tSQ"]);
    let ltt = trade_time_ms(merged);
    // On an index a volume or last quantity of 0 means the field does not
    // apply (an index has no trades of its own): left out, as one not sent.
    let index = matches!(sub.exchange.as_str(), "NSE_INDEX" | "BSE_INDEX");
    let none_on_index = |k: &str| index && int(g(k)) == 0;
    let mut absent = AbsentFields::default();
    for (missing, field) in [
        (g("ltp").is_none(), QuoteField::Ltp),
        (g("o").is_none(), QuoteField::Open),
        (g("h").is_none(), QuoteField::High),
        (g("l").is_none(), QuoteField::Low),
        (g("c").is_none(), QuoteField::Close),
        (
            g("vol").is_none() || none_on_index("vol"),
            QuoteField::Volume,
        ),
        (
            g("ltq").is_none() || none_on_index("ltq"),
            QuoteField::LastQuantity,
        ),
        (g("avgPr").is_none(), QuoteField::AveragePrice),
        (tbq.is_none(), QuoteField::TotalBuyQuantity),
        (tsq.is_none(), QuoteField::TotalSellQuantity),
        (ltt.is_none(), QuoteField::LastTradeTime),
    ] {
        if missing {
            absent = absent.with(field);
        }
    }
    let mut tick = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode: FeedMode::Quote.code(),
        ltp: num(g("ltp")),
        open: num(g("o")),
        high: num(g("h")),
        low: num(g("l")),
        close: num(g("c")),
        volume: int(g("vol")),
        average_price: num(g("avgPr")),
        last_quantity: int(g("ltq")),
        total_buy_quantity: int(tbq),
        total_sell_quantity: int(tsq),
        oi: int(g("oI")),
        change: round2(num(g("ch"))),
        change_percent: round2(num(g("chPer"))),
        last_trade_time_ms: ltt.unwrap_or(0),
        timestamp_ms: ts,
        absent,
        ..Default::default()
    };
    // Totals Samco's streams no longer hold (the book stream's go when the
    // last Depth holder leaves) are cleared from what the bridge keeps for
    // the instrument, not left standing in for the quote stream's.
    for (missing, field) in [
        (tbq.is_none(), QuoteField::TotalBuyQuantity),
        (tsq.is_none(), QuoteField::TotalSellQuantity),
    ] {
        if missing {
            tick.mark_cleared(field);
        }
    }
    if g("oI").is_some() {
        tick.mark_carried(QuoteField::OpenInterest);
    }
    let (bid_price, bid_quantity) = best_level(merged, "bidValues", "bPr", "bSz");
    let (ask_price, ask_quantity) = best_level(merged, "askValues", "aPr", "aSz");
    tick.extras.bid_price = bid_price;
    tick.extras.bid_quantity = bid_quantity;
    tick.extras.ask_price = ask_price;
    tick.extras.ask_quantity = ask_quantity;
    // Samco's own change, a 0 included; else the one the previous close
    // gives (none without a close).
    if g("ch").is_some() || g("chPer").is_some() {
        tick.mark_carried(QuoteField::Change);
    } else {
        tick.derive_change();
    }
    tick
}

/// The best level of a book side, as the web's `best_bid_price` and
/// `best_bid_quantity` (`samcoWebSocket._normalize_market_data`): the
/// book's first level once Samco has sent one, else the quote's own
/// (`bPr`/`bSz`, `aPr`/`aSz`); `None` for what Samco has sent neither of
/// (the web sends 0).
fn best_level(
    merged: &Map<String, Value>,
    side: &str,
    price: &str,
    quantity: &str,
) -> (Option<f64>, Option<i64>) {
    let top = merged
        .get(side)
        .and_then(Value::as_array)
        .and_then(|levels| levels.first());
    match top {
        Some(l) => (Some(num(l.get("price"))), Some(int(l.get("qty")))),
        None => (
            merged.get(price).map(|v| num(Some(v))),
            merged.get(quantity).map(|v| int(Some(v))),
        ),
    }
}

/// Whether Samco has sent the instrument's book (a quote2 frame).
fn has_book(merged: &Map<String, Value>) -> bool {
    merged.contains_key("bidValues") || merged.contains_key("askValues")
}

/// The book for a depth subscriber, from the merged ladder: the levels
/// Samco sent and no others (a cleared side is empty, never five zero
/// levels), its totals once Samco has sent them, and no price of its own
/// (quote2 carries none: the frame takes the instrument's last trade).
fn book(sub: &SubInfo, merged: &Map<String, Value>, ts: i64) -> NormalizedDepth {
    let levels = |k: &str| -> Vec<DepthLevel> {
        merged
            .get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|l| DepthLevel {
                        price: num(l.get("price")),
                        quantity: int(l.get("qty")),
                        orders: int(l.get("no")),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let tbq = first_present(merged, &["tbq", "tBQ"]);
    let tsq = first_present(merged, &["taq", "tSQ"]);
    let mut absent = AbsentFields::of(&[QuoteField::Ltp]);
    if tbq.is_none() {
        absent = absent.with(QuoteField::TotalBuyQuantity);
    }
    if tsq.is_none() {
        absent = absent.with(QuoteField::TotalSellQuantity);
    }
    NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        ltp: 0.0,
        buy: levels("bidValues"),
        sell: levels("askValues"),
        total_buy_quantity: int(tbq),
        total_sell_quantity: int(tsq),
        timestamp_ms: ts,
        absent,
    }
}

impl BrokerFeed for SamcoFeed {
    fn broker(&self) -> &'static str {
        "samco"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let mut req = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Samco feed address is invalid".into()))?;
        let v = HeaderValue::from_str(self.token.expose()).map_err(|_| {
            AppError::Auth("Your Samco session is not usable. Connect to Samco again.".into())
        })?;
        req.headers_mut().insert("x-session-token", v);
        Ok(req)
    }

    /// Fields from a dead connection must not survive into the next one
    /// (web `_cleanup_connection_state`): a `quote2` frame after a
    /// reconnect would otherwise show the old price and trade time. The
    /// subscriptions stay; the manager replays them.
    fn on_connected(&mut self) -> Vec<Message> {
        self.state.clear();
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut added = false;
        for s in subs {
            if let Some(k) = self.key(s) {
                self.subs.insert(
                    k,
                    SubInfo {
                        symbol: s.symbol.clone(),
                        exchange: s.exchange.clone(),
                        mode: s.mode,
                    },
                );
                added = true;
            }
        }
        if !added {
            return Vec::new();
        }
        self.full_set_frames()
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut quote_keys = Vec::new();
        let mut depth_keys = Vec::new();
        for s in subs {
            let Some(k) = self.key(s) else { continue };
            if let Some(info) = self.subs.remove(&k) {
                if info.mode == FeedMode::Depth {
                    depth_keys.push(k.clone());
                }
                quote_keys.push(k.clone());
            }
            self.state.remove(&k);
        }
        if quote_keys.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if !depth_keys.is_empty() {
            out.push(request_frame("quote2", "unsubscribe", &depth_keys));
        }
        out.push(request_frame("quote", "unsubscribe", &quote_keys));
        out.extend(self.full_set_frames());
        out
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        // Replace semantics: drop the depth stream if leaving depth, then
        // restate the full set at the new mode.
        let Some(k) = self.key(new) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if old.mode == FeedMode::Depth && new.mode != FeedMode::Depth {
            out.push(request_frame(
                "quote2",
                "unsubscribe",
                std::slice::from_ref(&k),
            ));
            // The book stream's fields go with it: its totals would
            // otherwise stand in for the quote stream's fresh ones (the
            // web's client drops the symbol's merged fields here).
            if let Some(st) = self.state.get_mut(&k) {
                for field in BOOK_FIELDS {
                    st.remove(*field);
                }
            }
        }
        self.subs.insert(
            k,
            SubInfo {
                symbol: new.symbol.clone(),
                exchange: new.exchange.clone(),
                mode: new.mode,
            },
        );
        out.extend(self.full_set_frames());
        out
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_text(t),
            Message::Binary(b) => match std::str::from_utf8(b) {
                Ok(t) => self.parse_text(t),
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// The web pings the feed every 30 s, answered within 10 s
    /// (`samcoWebSocket.py`: `PING_INTERVAL`, `PING_TIMEOUT`); its own
    /// heartbeat thread only watches for silence and sends nothing.
    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((WEB_PING_INTERVAL, ping()))
    }

    fn heartbeat_reply_timeout(&self) -> Option<Duration> {
        Some(WEB_PING_TIMEOUT)
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}
