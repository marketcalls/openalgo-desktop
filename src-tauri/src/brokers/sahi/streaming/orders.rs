//! Sahi order updates (web `broker/sahi/streaming/sahi_order_adapter.py`,
//! openalgo#2474): `wss://direct.sahi.com/v1/stream/orders`, on the shared
//! feed manager as the order socket.
//!
//! * The upgrade carries the REST Bearer token and the User-Agent Sahi's
//!   CloudFront requires (`feed_request`, as the market data stream's).
//!   There is no login or subscribe frame: Sahi pushes as soon as the
//!   socket is open, scoped to the token's account. Order streams are
//!   counted apart from the tick stream (five per account), so this socket
//!   takes no market data connection.
//! * Each frame is one JSON order object shaped like an element of
//!   `GET /v1/orders`, mapped by the order book's own mapping
//!   (`mapping::order_update`): paise to rupees, Sahi's codes and statuses.
//!   A frame that is not an order is ignored.
//! * A token Sahi no longer accepts is refused at the upgrade with 401 or
//!   403, which stops the order feed until the trader logs in again. When
//!   Sahi closes with 4401 (token expired) the feed connects again, as the
//!   web's adapter does, and that upgrade is the one refused.
//! * Pings as the web's order sockets send them: every 20 s, answered
//!   within 10 s. Sahi's own pings are answered by the socket.

use super::feed_request;
use crate::brokers::common::streaming::{
    ping, BrokerFeed, FeedEvent, FeedSubscription, Message, OrderUpdate, WsRequest,
    WEB_ORDER_PING_INTERVAL, WEB_PING_TIMEOUT,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::sahi::mapping;
use crate::error::Result;
use crate::security::Secret;
use std::time::Duration;

pub struct SahiOrderFeed {
    /// The session's REST token, sent on the upgrade (`Debug` redacted).
    access_token: Secret,
    symbols: SymbolResolver,
    url: String,
}

impl std::fmt::Debug for SahiOrderFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SahiOrderFeed")
            .field("access_token", &self.access_token)
            .field("url", &self.url)
            .finish()
    }
}

impl SahiOrderFeed {
    pub fn new(access_token: &str, symbols: SymbolResolver, url: String) -> Self {
        Self {
            access_token: Secret::new(access_token),
            symbols,
            url,
        }
    }

    /// The update one text frame carries (web `normalize`), if any.
    fn update(&self, text: &str) -> Option<OrderUpdate> {
        let row = serde_json::from_str(text).ok()?;
        mapping::order_update(&row, &self.symbols)
    }
}

impl BrokerFeed for SahiOrderFeed {
    fn broker(&self) -> &'static str {
        "sahi"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        feed_request(&self.url, self.access_token.expose())
    }

    /// Sahi pushes the account's orders unasked; the feed holds no
    /// instruments.
    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        let update = match msg {
            Message::Text(t) => self.update(t),
            Message::Binary(b) => self.update(&String::from_utf8_lossy(b)),
            _ => None,
        };
        update.map(FeedEvent::OrderUpdate).into_iter().collect()
    }

    /// The web's order sockets ping every 20 s, answered within 10 s
    /// (`websocket_proxy/order_adapter.py`; Sahi keeps the default).
    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((WEB_ORDER_PING_INTERVAL, ping()))
    }

    fn heartbeat_reply_timeout(&self) -> Option<Duration> {
        Some(WEB_PING_TIMEOUT)
    }
}

#[cfg(test)]
mod tests;
