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
//!   bSz, aPr, aSz, tBQ/tbq, tSQ/taq, lTrdT/ltt, bidValues[{price,qty,no}],
//!   askValues`.
//! * Indices subscribe by `listingId` (`-23`), no suffix
//!   (`samcoWebSocket.py:849-863`); ids come from `/quote/indexQuote` and
//!   are shared with the REST adapter through [`ListingIds`].

use super::mapping::{int, num, text};
use super::ListingIds;
use crate::brokers::common::streaming::{
    now_ms, round2, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct SamcoFeed {
    url: String,
    token: String,
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
            token: unquote(session_token),
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

    fn event(&mut self, streaming_type: &str, data: &Map<String, Value>) -> Vec<FeedEvent> {
        let key = {
            let k = text(data.get("symbol"));
            if k.is_empty() {
                text(data.get("sym"))
            } else {
                k
            }
        };
        let Some(sub) = self.subs.get(&key).cloned() else {
            return Vec::new();
        };
        let merged = {
            let st = self.state.entry(key).or_default();
            for (k, v) in data {
                let empty = v.is_null() || v.as_str() == Some("");
                if !empty {
                    st.insert(k.clone(), v.clone());
                }
            }
            st.clone()
        };
        let g = |k: &str| merged.get(k);
        let levels = |k: &str| -> Vec<DepthLevel> {
            g(k).and_then(Value::as_array)
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
        let bids = levels("bidValues");
        let asks = levels("askValues");
        let total = |a: &str, b: &str| {
            let x = g(a).filter(|v| super::mapping::truthy(Some(v)));
            int(x.or_else(|| g(b)))
        };
        let tbq = total("tbq", "tBQ");
        let tsq = total("taq", "tSQ");
        let ts = now_ms();
        let mut tick = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: num(g("ltp")),
            open: num(g("o")),
            high: num(g("h")),
            low: num(g("l")),
            close: num(g("c")),
            volume: int(g("vol")),
            average_price: num(g("avgPr")),
            last_quantity: int(g("ltq")),
            total_buy_quantity: tbq,
            total_sell_quantity: tsq,
            oi: int(g("oI")),
            change: round2(num(g("ch"))),
            change_percent: round2(num(g("chPer"))),
            last_trade_time_ms: 0,
            timestamp_ms: ts,
        };
        if tick.change == 0.0 && tick.change_percent == 0.0 {
            tick.derive_change();
        }
        let _ = streaming_type;
        let mut out = vec![FeedEvent::Tick(tick.clone())];
        if sub.mode == FeedMode::Depth {
            let pad = |mut v: Vec<DepthLevel>| {
                v.truncate(5);
                v.resize(5, DepthLevel::default());
                v
            };
            out.push(FeedEvent::Depth(NormalizedDepth {
                symbol: sub.symbol,
                exchange: sub.exchange,
                ltp: tick.ltp,
                buy: pad(bids),
                sell: pad(asks),
                total_buy_quantity: tbq,
                total_sell_quantity: tsq,
                timestamp_ms: ts,
            }));
        }
        out
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
            if matches!(stype.as_str(), "quote" | "quote2" | "marketDepth") {
                if let Some(p) = payload {
                    out.extend(self.event(&stype, &p));
                }
            }
        }
        out
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
        let v = HeaderValue::from_str(&self.token).map_err(|_| {
            AppError::Auth("Your Samco session is not usable. Connect to Samco again.".into())
        })?;
        req.headers_mut().insert("x-session-token", v);
        Ok(req)
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

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}
