//! Tradejini order updates (web `streaming/tradejini_order_adapter.py`,
//! marketcalls/openalgo#2477): the `orders` events of the CubePlus stream,
//! on a socket of their own.
//!
//! The socket is the market feed's address (`streaming::stream_url`): the
//! API key and access token in the URL authenticate it, with no login frame.
//! Once it opens the feed asks for the order events as the SDK does
//! (`NxtradStream.subscribeEvents`). They come in the market feed's binary
//! frames (`streaming::decode_message`) as packet 15, each a JSON object
//! routed by `evntType`; an `orders` event becomes an `order_update` in
//! OpenAlgo's constants, through Tradejini's own maps (`mapping`), as the
//! web maps it with the broker's `transform_data`. Tradejini allows five
//! sockets a login, so this one does not displace the market feed.
//!
//! Tradejini refuses a session two ways, and the same token is refused
//! again on every reconnect, so the feed stops until the next login: an
//! auth packet (13) other than 1, whose frame's events go with it, and a
//! close with code 4001.
//!
//! Where the desktop departs from the web: the web reads the access token
//! again before each reconnect; the desktop's changes only with a login,
//! which starts a new feed. A number in an event that is not finite skips
//! that event, where the web publishes it or drops the frame. A packet cut
//! short costs only itself (`streaming::event_messages`).

use super::mapping;
use super::streaming::{decode_message, stream_url, Packet};
use crate::brokers::common::python::truthy;
use crate::brokers::common::streaming::{
    ping, BrokerFeed, Ended, FeedEvent, FeedSubscription, Message, OrderUpdate, Reconnect,
    StopKind, WsRequest, WEB_ORDER_PING_INTERVAL, WEB_PING_TIMEOUT,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::error::{AppError, Result};
use crate::security::secret::Secret;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// The close code of an expired or invalid token (`12-websocket-sdks.md`,
/// "Unauthorized Access").
const UNAUTHORIZED_CLOSE: u16 = 4001;

/// What the trader is told when Tradejini refuses the session.
const REFUSED: &str = "Tradejini refused the order update connection. Log in to Tradejini again.";

/// The Tradejini order-update feed.
pub struct TradejiniOrderFeed {
    /// Carries the access token in its query.
    url: Secret,
    symbols: SymbolResolver,
}

impl TradejiniOrderFeed {
    /// `base` is the stream endpoint (`streaming::STREAM_URL`; tests pass a
    /// local one).
    pub fn new(base: &str, api_key: &str, access_token: &str, symbols: SymbolResolver) -> Self {
        Self {
            url: stream_url(base, api_key, access_token),
            symbols,
        }
    }

    /// One binary frame (web `normalize_frame`): every order event in it,
    /// or only the refusal when an auth packet says Tradejini did not take
    /// the session. An accepted session is quiet, as on the web.
    fn parse_binary(&self, m: &[u8]) -> Vec<FeedEvent> {
        let packets = decode_message(m);
        let refused = packets.iter().find_map(|p| match p {
            Packet::Auth(status) if *status != 1 => Some(*status),
            _ => None,
        });
        if let Some(status) = refused {
            tracing::warn!(
                "Tradejini order updates refused the session (status {})",
                status
            );
            return vec![FeedEvent::AuthFailed(REFUSED.into())];
        }
        packets
            .iter()
            .filter_map(|p| match p {
                Packet::Events(events) => Some(events),
                _ => None,
            })
            .flatten()
            .filter_map(|event| order_update(event, &self.symbols))
            .map(FeedEvent::OrderUpdate)
            .collect()
    }
}

/// One event as an order update (web `normalize_event`), or `None` for
/// anything else: text that is not a JSON object, an event other than
/// `orders` (positions and trades are not asked for), or an order event
/// with a number Python's `float()` would refuse, which the web skips and
/// keeps the rest of its frame.
fn order_update(event: &str, symbols: &SymbolResolver) -> Option<OrderUpdate> {
    let e: Value = serde_json::from_str(event).ok()?;
    if !e.is_object() || text(&e, "evntType").to_lowercase() != "orders" {
        return None;
    }
    let raw_status = text(&e, "status").trim().to_lowercase().replace('_', " ");
    let order_status = mapping::order_status(&raw_status);
    let exchange = text(&e, "exch");
    let mut u = OrderUpdate {
        orderid: text(&e, "orderId"),
        symbol: openalgo_symbol(&e, &exchange, symbols),
        action: action(&text(&e, "side")),
        pricetype: match text(&e, "type") {
            t if t.is_empty() => t,
            t => mapping::reverse_order_type(&t),
        },
        product: match text(&e, "product") {
            p if p.is_empty() => p,
            p => mapping::reverse_product(&p).map_or_else(|| p.to_uppercase(), String::from),
        },
        rejection_reason: if order_status == "rejected" {
            text(&e, "reason").trim().to_string()
        } else {
            String::new()
        },
        exchange,
        order_status,
        ..OrderUpdate::default()
    };
    // The documented event names no fill quantity, average price or
    // trigger price; they are read under the order book's names when
    // Tradejini sends them, and otherwise left at the web's defaults.
    if let Some(q) = number(&e, "qty").ok()? {
        u.quantity = q.trunc() as i64;
    }
    if let Some(p) = number(&e, "price").ok()? {
        u.price = p;
    }
    if let Some(p) = number(&e, "trigPrice").ok()? {
        u.trigger_price = p;
    }
    if let Some(p) = number(&e, "avgPrice").ok()? {
        u.average_price = p;
    }
    if let Some(q) = number(&e, "fillQty").ok()? {
        u.filled_quantity = q.trunc() as i64;
    }
    if let Some(q) = number(&e, "pendingQty").ok()? {
        u.pending_quantity = q.trunc() as i64;
    }
    Some(u)
}

/// `str(event.get(key) or "")`: a missing or empty value is empty text.
fn text(e: &Value, key: &str) -> String {
    if truthy(e.get(key)) {
        mapping::s(e, key)
    } else {
        String::new()
    }
}

/// A number Python's `float()` would refuse, or one that is not finite.
struct Unreadable;

/// `float()` of a field the event carries (web `_present`): `None` when it
/// is missing, null or blank text.
fn number(e: &Value, key: &str) -> std::result::Result<Option<f64>, Unreadable> {
    match e.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        v => mapping::num_strict(v, 0.0)
            .filter(|x| x.is_finite())
            .map(Some)
            .ok_or(Unreadable),
    }
}

/// The order book's lookup (`symId` first, then `trdSym`, on the event's
/// exchange), else the broker's own symbol, as the web reports it when the
/// master has no match.
fn openalgo_symbol(e: &Value, exchange: &str, symbols: &SymbolResolver) -> String {
    let sym_id = text(e, "symId");
    let trd_sym = text(e, "trdSym");
    let found = mapping::resolve(symbols, &[&sym_id, &trd_sym], exchange);
    match found {
        Some(symbol) => symbol,
        None if trd_sym.is_empty() => sym_id,
        None => trd_sym,
    }
}

/// web `_ACTION_MAP`: the REST book's `buy`/`sell` and the docs' `b`/`s`.
fn action(side: &str) -> String {
    match side.to_lowercase().as_str() {
        "buy" | "b" => "BUY".into(),
        "sell" | "s" => "SELL".into(),
        _ => side.to_uppercase(),
    }
}

impl BrokerFeed for TradejiniOrderFeed {
    fn broker(&self) -> &'static str {
        "tradejini"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .expose()
            .into_client_request()
            .map_err(|_| AppError::Internal("Tradejini order feed address is invalid".into()))
    }

    /// `NxtradStream.subscribeEvents(["orders"])`: JSON and a newline.
    fn on_connected(&mut self) -> Vec<Message> {
        let request = json!({"type": "event", "action": "sub", "events": ["orders"]});
        vec![Message::Text(format!("{}\n", request))]
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            _ => Vec::new(),
        }
    }

    /// The web's order sockets ping every 20 s and drop one that does not
    /// answer within 10 s (`websocket_proxy/order_adapter.py`).
    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((WEB_ORDER_PING_INTERVAL, ping()))
    }

    fn heartbeat_reply_timeout(&self) -> Option<Duration> {
        Some(WEB_PING_TIMEOUT)
    }

    /// 4001 is an expired or invalid token, refused again on every
    /// reconnect: stop until the next login. Any other end reconnects.
    fn on_ended(&mut self, ended: Ended<'_>) -> Reconnect {
        match ended {
            Ended::Closed {
                code: Some(UNAUTHORIZED_CLOSE),
                ..
            } => {
                tracing::warn!("Tradejini closed the order update connection: session refused");
                Reconnect::Stop {
                    kind: StopKind::Auth,
                    message: REFUSED.into(),
                }
            }
            _ => Reconnect::Backoff,
        }
    }
}
