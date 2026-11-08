//! Sahi market-data feed (web `broker/sahi/streaming/`), on the shared
//! `websocket::WebSocketManager`.
//!
//! * `wss://direct.sahi.com/v1/stream/marketdata`, authorised by
//!   `Authorization: Bearer <token>` on the upgrade. Sahi's CloudFront
//!   answers 403 to an upgrade without a User-Agent, so one is always sent.
//! * Frames are protobuf (`proto.rs`). Instruments are Sahi's
//!   `instrument_id`, the master's `token` column; a subscription names a
//!   mode and ids, with no segment.
//! * Prices arrive in paise. The stream carries no open, high, low, close,
//!   last quantity, average price or total buy and sell quantity, and an
//!   index tick no trade time: those are left out of the frames, never sent
//!   as 0 or derived (web openalgo#2376).
//! * Quote and Depth frames carry the daily price band as `upper_circuit`
//!   and `lower_circuit` (web `sahi_mapping.py` `build_quote_payload`,
//!   audit P-11). A tick without a band leaves it out; the web sends 0.0.
//! * The server pings every 30 s and closes with 4408 when no pong comes;
//!   tungstenite answers its pings, so the feed sends no heartbeat. The
//!   pings also keep the manager's stall watchdog quiet.
//! * A token Sahi no longer accepts (expired, or its key revoked) is
//!   refused at the upgrade with HTTP 401 or 403 (docs.sahi.com), which
//!   stops the feed until the trader logs in again. A socket Sahi closes
//!   (4401 or 4403 for a token revoked mid-session, 4408 ping timeout,
//!   4429 too many connections) is reconnected with the manager's capped
//!   backoff, since the manager does not read close codes; after a
//!   revocation that next upgrade is the 401 or 403 that stops it. The web
//!   (`sahi_websocket.py`) stops on the close code itself and waits 60 to
//!   300 s after a 4429.

mod proto;
#[cfg(test)]
mod tests;

use crate::brokers::common::streaming::{
    now_ms, AbsentFields, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
    NormalizedDepth, NormalizedTick, QuoteField, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use proto::{ServerMessage, Tick, TickKind};
use std::collections::HashMap;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// The User-Agent CloudFront requires on the upgrade (web `baseurl.py`).
const USER_AGENT: &str = "openalgo-sahi/1.0";

/// Sahi refuses more instruments than this on one connection (docs,
/// "Per-connection limits"); the subscription map is bounded by it.
pub(crate) const MAX_INSTRUMENTS: usize = 1000;

/// Ids per Subscribe / Unsubscribe frame (web `_RESUBSCRIBE_CHUNK`).
const IDS_PER_FRAME: usize = 500;

/// OpenAlgo depth is 5 levels; the stream may send up to 20.
const DEPTH_LEVELS: usize = 5;

/// What the stream never carries (web `sahi_mapping.py`); with no previous
/// close there is no change either.
const NOT_ON_THE_STREAM: AbsentFields = AbsentFields::of(&[
    QuoteField::Open,
    QuoteField::High,
    QuoteField::Low,
    QuoteField::Close,
    QuoteField::Change,
    QuoteField::AveragePrice,
    QuoteField::LastQuantity,
    QuoteField::TotalBuyQuantity,
    QuoteField::TotalSellQuantity,
]);

/// Close codes that mean the token no longer opens the stream.
const CLOSE_TOKEN_EXPIRED: u16 = 4401;
const CLOSE_FORBIDDEN: u16 = 4403;

/// What a subscribed instrument is published as.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct SahiFeed {
    access_token: String,
    symbols: SymbolResolver,
    url: String,
    /// Sahi instrument id -> the OpenAlgo instrument it was subscribed for.
    /// Removed on unsubscribe; never larger than `MAX_INSTRUMENTS`.
    subs: HashMap<i64, SubInfo>,
}

impl std::fmt::Debug for SahiFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SahiFeed")
            .field("access_token", &"[REDACTED]")
            .field("url", &self.url)
            .field("subscribed", &self.subs.len())
            .finish()
    }
}

/// OpenAlgo mode -> Sahi `Mode`.
fn sahi_mode(mode: FeedMode) -> u64 {
    match mode {
        FeedMode::Ltp => proto::MODE_LTP,
        FeedMode::Quote => proto::MODE_QUOTE,
        FeedMode::Depth => proto::MODE_DEPTH,
    }
}

/// The Sahi `ExchangeSegment` ticks of an OpenAlgo exchange carry; indices
/// stream on their cash segment.
fn segment_of(exchange: &str) -> Option<u64> {
    match exchange {
        "NSE" | "NSE_INDEX" => Some(proto::SEGMENT_NSE),
        "BSE" | "BSE_INDEX" => Some(proto::SEGMENT_BSE),
        "NFO" => Some(proto::SEGMENT_NFO),
        "BFO" => Some(proto::SEGMENT_BFO),
        _ => None,
    }
}

/// Integer paise to rupees.
pub(crate) fn rupees(paise: i64) -> f64 {
    paise as f64 / 100.0
}

/// One side of the daily price band in rupees, or none when the tick left
/// it off: the band is a proto3 scalar, so an absent one reads as 0, and
/// no instrument trades in a band of 0 (the web publishes 0.0 then).
fn band(paise: i64) -> Option<f64> {
    (paise != 0).then(|| rupees(paise))
}

/// Whether a handshake status (or, should the manager pass one, a close
/// code) means the token is dead.
pub(crate) fn is_auth_close(code: u16) -> bool {
    matches!(code, 401 | 403 | CLOSE_TOKEN_EXPIRED | CLOSE_FORBIDDEN)
}

/// A depth side in rupees, truncated and padded to 5 levels: Sahi sends
/// only levels holding orders, OpenAlgo depth is always 5 rows.
fn depth_side(levels: &[proto::Level]) -> Vec<DepthLevel> {
    let mut side: Vec<DepthLevel> = levels
        .iter()
        .take(DEPTH_LEVELS)
        .map(|l| DepthLevel {
            price: rupees(l.price),
            quantity: l.qty,
            orders: l.orders,
        })
        .collect();
    side.resize(DEPTH_LEVELS, DepthLevel::default());
    side
}

fn ack_name(action: u64) -> &'static str {
    match action {
        proto::ACK_SUBSCRIBE => "subscribe",
        proto::ACK_UNSUBSCRIBE => "unsubscribe",
        proto::ACK_MODE_CHANGE => "mode change",
        _ => "unknown",
    }
}

/// The handshake: the stream URL with the Bearer token and User-Agent.
pub(crate) fn feed_request(url: &str, access_token: &str) -> Result<WsRequest> {
    let mut req = url
        .into_client_request()
        .map_err(|_| AppError::Internal("Sahi market data address is invalid".into()))?;
    let auth = HeaderValue::from_str(&format!("Bearer {}", access_token)).map_err(|_| {
        AppError::Auth(
            "Your Sahi session is not usable for live data. Log in to Sahi again.".into(),
        )
    })?;
    let h = req.headers_mut();
    h.insert("Authorization", auth);
    h.insert("User-Agent", HeaderValue::from_static(USER_AGENT));
    Ok(req)
}

impl SahiFeed {
    pub fn new(access_token: String, symbols: SymbolResolver, url: String) -> Self {
        Self {
            access_token,
            symbols,
            url,
            subs: HashMap::new(),
        }
    }

    /// The Sahi id of a subscription: its token, else the master's token
    /// for its symbol (a subscription built before the master reloaded).
    fn instrument_id(&self, s: &FeedSubscription) -> Option<i64> {
        if let Ok(id) = s.token.trim().parse::<i64>() {
            return Some(id);
        }
        let id = self
            .symbols
            .by_symbol(&s.exchange, &s.symbol)
            .and_then(|row| row.token.trim().parse::<i64>().ok());
        if id.is_none() {
            tracing::warn!("No Sahi instrument id for {}:{}", s.exchange, s.symbol);
        }
        id
    }

    /// Record subscriptions; returns the ids to send, grouped by mode.
    fn register(&mut self, subs: &[FeedSubscription]) -> Vec<(FeedMode, Vec<i64>)> {
        let mut by_mode: Vec<(FeedMode, Vec<i64>)> = Vec::new();
        for s in subs {
            let Some(id) = self.instrument_id(s) else {
                continue;
            };
            if !self.subs.contains_key(&id) && self.subs.len() >= MAX_INSTRUMENTS {
                tracing::warn!(
                    "Sahi allows {} instruments per connection; {}:{} not subscribed",
                    MAX_INSTRUMENTS,
                    s.exchange,
                    s.symbol
                );
                continue;
            }
            self.subs.insert(
                id,
                SubInfo {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    mode: s.mode,
                },
            );
            match by_mode.iter_mut().find(|(m, _)| *m == s.mode) {
                Some((_, ids)) => ids.push(id),
                None => by_mode.push((s.mode, vec![id])),
            }
        }
        by_mode
    }

    /// Forget subscriptions; returns the ids to unsubscribe.
    fn release(&mut self, subs: &[FeedSubscription]) -> Vec<i64> {
        let ids: Vec<i64> = subs.iter().filter_map(|s| self.instrument_id(s)).collect();
        ids.into_iter()
            .filter(|id| self.subs.remove(id).is_some())
            .collect()
    }

    /// The subscription a tick belongs to. A tick whose segment does not
    /// match the subscription's exchange is dropped rather than published
    /// under another instrument.
    fn lookup(&self, t: &Tick) -> Option<&SubInfo> {
        let sub = self.subs.get(&t.instrument_id)?;
        if t.exchange_segment != 0 && segment_of(&sub.exchange) != Some(t.exchange_segment) {
            tracing::debug!(
                id = t.instrument_id,
                segment = t.exchange_segment,
                "Sahi tick on a segment other than its subscription dropped"
            );
            return None;
        }
        Some(sub)
    }

    /// Normalised events for one tick (web `build_payloads`). A tick serves
    /// its own level and below: a quote tick that arrives before a pending
    /// upgrade to depth is published as a quote, with no depth.
    fn events(&self, kind: TickKind, t: &Tick) -> Vec<FeedEvent> {
        let Some(sub) = self.lookup(t) else {
            return Vec::new();
        };
        let served = sub.mode.min(match kind {
            TickKind::Ltp => FeedMode::Ltp,
            TickKind::Quote => FeedMode::Quote,
            TickKind::Depth => FeedMode::Depth,
        });
        let now = now_ms();
        let ltp = rupees(t.last_trade_price);
        // An index tick has no trade time: none is published, rather than
        // the time it was received.
        let absent = match t.last_trade_time {
            Some(_) => NOT_ON_THE_STREAM,
            None => NOT_ON_THE_STREAM.with(QuoteField::LastTradeTime),
        };
        let mut tick = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: served.code(),
            ltp,
            last_trade_time_ms: t.last_trade_time.unwrap_or(0),
            timestamp_ms: now,
            absent,
            ..Default::default()
        };
        if served >= FeedMode::Quote {
            tick.volume = t.volume_today;
            tick.oi = t.open_interest.unwrap_or(0);
            tick.extras.upper_circuit = band(t.dpr_upper);
            tick.extras.lower_circuit = band(t.dpr_lower);
        }
        let mut out = Vec::with_capacity(2);
        out.push(FeedEvent::Tick(tick));
        if served == FeedMode::Depth {
            out.push(FeedEvent::Depth(NormalizedDepth {
                symbol: sub.symbol.clone(),
                exchange: sub.exchange.clone(),
                ltp,
                buy: depth_side(&t.best_buys),
                sell: depth_side(&t.best_sells),
                total_buy_quantity: 0,
                total_sell_quantity: 0,
                timestamp_ms: now,
                ..Default::default()
            }));
        }
        out
    }

    fn decode(&self, data: &[u8]) -> Vec<FeedEvent> {
        match proto::decode_server_message(data) {
            Ok(ServerMessage::Tick(kind, t)) => self.events(kind, &t),
            Ok(ServerMessage::Ack(a)) => {
                tracing::debug!(
                    "Sahi acknowledged {} of {} instruments",
                    ack_name(a.action),
                    a.instrument_ids.len()
                );
                Vec::new()
            }
            Ok(ServerMessage::Error(e)) => {
                // Non-fatal by Sahi's docs (unknown id, mode change on an
                // id not subscribed): the connection stays open.
                tracing::warn!("Sahi stream reported {}: {}", e.code, e.message);
                Vec::new()
            }
            Ok(ServerMessage::Empty) => Vec::new(),
            Err(e) => {
                tracing::warn!(
                    "Dropped a malformed Sahi frame ({} bytes): {}",
                    data.len(),
                    e.0
                );
                Vec::new()
            }
        }
    }
}

fn chunked(ids: &[i64], encode: impl Fn(&[i64]) -> Vec<u8>) -> Vec<Message> {
    ids.chunks(IDS_PER_FRAME)
        .map(|c| Message::Binary(encode(c)))
        .collect()
}

impl BrokerFeed for SahiFeed {
    fn broker(&self) -> &'static str {
        "sahi"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        feed_request(&self.url, &self.access_token)
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.register(subs)
            .into_iter()
            .flat_map(|(mode, ids)| chunked(&ids, |c| proto::encode_subscribe(sahi_mode(mode), c)))
            .collect()
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let ids = self.release(subs);
        chunked(&ids, proto::encode_unsubscribe)
    }

    /// Sahi changes an instrument's mode in place (`ModeChange`), so no
    /// unsubscribe gap opens between the two modes.
    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        let Some(id) = self.instrument_id(new) else {
            return Vec::new();
        };
        if !self.subs.contains_key(&id) {
            self.release(std::slice::from_ref(old));
            return self.subscribe_frames(std::slice::from_ref(new));
        }
        self.subs.insert(
            id,
            SubInfo {
                symbol: new.symbol.clone(),
                exchange: new.exchange.clone(),
                mode: new.mode,
            },
        );
        vec![Message::Binary(proto::encode_mode_change(
            sahi_mode(new.mode),
            &[id],
        ))]
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.decode(b),
            _ => Vec::new(),
        }
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }

    fn is_auth_failure(&self, http_status: Option<u16>) -> bool {
        http_status.is_some_and(is_auth_close)
    }
}
