//! Broker streaming contract (audit Part C.3).
//!
//! A `BrokerFeed` only translates: it says how to open the socket, which
//! frames to send, and how to turn broker frames into normalised events.
//! The socket, reconnects, backoff, watchdog and re-subscription live once,
//! in `crate::websocket::WebSocketManager`.
//!
//! Brokers whose connect needs async work (Upstox's single-use authorize
//! call, Groww's socket token) do it in `prepare`, which the manager awaits
//! before every (re)connect. Protocol replies (NATS `CONNECT` and `PONG`)
//! are returned from `parse` as `FeedEvent::Reply` and written to the
//! socket by the manager.

use crate::brokers::types::DepthLevel;
use crate::error::Result;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
pub use tokio_tungstenite::tungstenite::handshake::client::Request as WsRequest;
pub use tokio_tungstenite::tungstenite::Message;

/// OpenAlgo subscription mode (web modes 1/2/3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum FeedMode {
    Ltp = 1,
    Quote = 2,
    Depth = 3,
}

impl FeedMode {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(FeedMode::Ltp),
            2 => Some(FeedMode::Quote),
            3 => Some(FeedMode::Depth),
            _ => None,
        }
    }

    pub fn code(self) -> u8 {
        self as u8
    }
}

/// One instrument subscription, already resolved against the symbol master.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedSubscription {
    /// OpenAlgo symbol and exchange; ticks are reported under these.
    pub symbol: String,
    pub exchange: String,
    /// Broker token, symbol and exchange from the master row.
    pub token: String,
    pub brsymbol: String,
    pub brexchange: String,
    pub mode: FeedMode,
    /// Requested depth levels (5 unless a client asked for 20/30/50).
    pub depth: u8,
}

impl FeedSubscription {
    /// Key that identifies the instrument regardless of mode.
    pub fn instrument_key(&self) -> (String, String) {
        (self.exchange.clone(), self.symbol.clone())
    }

    pub fn with_mode(&self, mode: FeedMode) -> Self {
        Self {
            mode,
            ..self.clone()
        }
    }
}

/// A normalised tick. Fields a mode does not carry are zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NormalizedTick {
    pub symbol: String,
    pub exchange: String,
    pub mode: u8,
    pub ltp: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    /// Previous close.
    pub close: f64,
    pub volume: i64,
    pub average_price: f64,
    pub last_quantity: i64,
    pub total_buy_quantity: i64,
    pub total_sell_quantity: i64,
    pub oi: i64,
    /// `ltp - close` when the broker does not send it.
    pub change: f64,
    pub change_percent: f64,
    /// Last trade time from the exchange, epoch ms (0 if not sent).
    pub last_trade_time_ms: i64,
    /// When this tick was produced, epoch ms.
    pub timestamp_ms: i64,
}

impl NormalizedTick {
    /// Fill `change` / `change_percent` from `ltp` and `close`.
    pub fn derive_change(&mut self) {
        if self.close > 0.0 && self.ltp > 0.0 {
            self.change = round2(self.ltp - self.close);
            self.change_percent = round2((self.ltp - self.close) / self.close * 100.0);
        }
    }
}

/// Normalised market depth.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NormalizedDepth {
    pub symbol: String,
    pub exchange: String,
    pub ltp: f64,
    pub buy: Vec<DepthLevel>,
    pub sell: Vec<DepthLevel>,
    pub total_buy_quantity: i64,
    pub total_sell_quantity: i64,
    pub timestamp_ms: i64,
}

/// A normalised order update (web `order_update` event fields).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OrderUpdate {
    pub orderid: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub price: f64,
    pub trigger_price: f64,
    pub pricetype: String,
    pub product: String,
    pub order_status: String,
    pub filled_quantity: i64,
    pub pending_quantity: i64,
    pub average_price: f64,
    pub rejection_reason: String,
}

/// What a feed produces from one broker frame.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedEvent {
    Tick(NormalizedTick),
    Depth(NormalizedDepth),
    OrderUpdate(OrderUpdate),
    /// Broker accepted the session (feeds that wait for an ack).
    AuthOk,
    /// Broker refused the session; the manager stops reconnecting.
    AuthFailed(String),
    Heartbeat,
    /// A frame to write back to the broker (protocol replies: NATS
    /// `CONNECT` after `INFO`, `PONG` after `PING`). Never published.
    Reply(Message),
}

/// Why `BrokerFeed::prepare` could not ready a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareError {
    /// The broker refused the stored login; the manager stops until the
    /// trader logs in again. The message is trader-facing.
    AuthFailed(String),
    /// Anything else (network, broker outage): back off and retry.
    Unavailable,
}

/// Where a broker's order updates come from.
pub enum OrderFeed {
    /// A dedicated order socket, run by its own `WebSocketManager`.
    Socket(Box<dyn BrokerFeed>),
    /// Updates produced by the adapter itself (an order-book poller). The
    /// adapter owns the producing task and stops it in `Broker::on_logout`;
    /// the channel closes with it.
    Stream(tokio::sync::mpsc::Receiver<OrderUpdate>),
}

impl std::fmt::Debug for OrderFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderFeed::Socket(s) => write!(f, "OrderFeed::Socket({})", s.broker()),
            OrderFeed::Stream(_) => f.write_str("OrderFeed::Stream"),
        }
    }
}

/// Events published to subscribers of the manager (shared, cheap to clone).
pub type MarketEvent = Arc<FeedEvent>;

/// The per-broker streaming adapter.
#[async_trait]
pub trait BrokerFeed: Send + Sync {
    /// Broker id, for logs.
    fn broker(&self) -> &'static str;

    /// Async work before every (re)connect, awaited by the manager inside
    /// its connect timeout: fetch a single-use socket URL, mint a socket
    /// token. `ws_request` is called right after it succeeds.
    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        Ok(())
    }

    /// URL and headers for the WebSocket handshake. Must not log secrets.
    fn ws_request(&self) -> Result<WsRequest>;

    /// A connect attempt failed before the handshake completed (`error` is
    /// for matching, never shown to the trader). Return true to retry once,
    /// at once, with a fresh `ws_request` (Groww drops the `nats`
    /// subprotocol when the server does not echo it); false backs off.
    fn on_connect_failed(&mut self, _error: &str) -> bool {
        false
    }

    /// Frames to send right after the socket opens (HSM auth, login).
    fn on_connected(&mut self) -> Vec<Message> {
        Vec::new()
    }

    /// Whether subscriptions must wait for `FeedEvent::AuthOk`.
    fn awaits_auth_ack(&self) -> bool {
        false
    }

    /// When the broker may never acknowledge (Groww's NATS `+OK` is
    /// optional), treat the session as accepted after this long.
    fn auth_ack_timeout(&self) -> Option<Duration> {
        None
    }

    /// Frames that subscribe these instruments (one per instrument, already
    /// at its effective mode). May be several frames (Kite needs two).
    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message>;

    /// Frames that unsubscribe these instruments (at the mode they were
    /// subscribed with).
    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message>;

    /// Frames that move an instrument from one effective mode to another.
    /// Default: unsubscribe the old mode, subscribe the new one.
    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        let mut v = self.unsubscribe_frames(std::slice::from_ref(old));
        v.extend(self.subscribe_frames(std::slice::from_ref(new)));
        v
    }

    /// Decode one frame.
    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent>;

    /// Client heartbeat, if the broker wants one.
    fn heartbeat(&self) -> Option<(Duration, Message)> {
        None
    }

    /// Depth levels the broker can stream.
    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }

    /// Whether a handshake/close failure means the token is no longer valid
    /// (stop reconnecting until the trader logs in again).
    fn is_auth_failure(&self, http_status: Option<u16>) -> bool {
        matches!(http_status, Some(401) | Some(403))
    }
}

/// A handshake request whose URL has no path (`wss://ws.kite.trade?api_key=..`)
/// would go out as `GET ?api_key=..`, which servers refuse. Give it the root
/// path, keeping the query and headers.
pub fn normalize_request(mut req: WsRequest) -> WsRequest {
    let pq = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("");
    if pq.starts_with('/') {
        return req;
    }
    let uri = req.uri().clone();
    let pq = match uri.query() {
        Some(q) => format!("/?{}", q),
        None => "/".to_string(),
    };
    let mut parts = uri.into_parts();
    if let Ok(p) = pq.parse() {
        parts.path_and_query = Some(p);
        if let Ok(u) = tokio_tungstenite::tungstenite::http::Uri::from_parts(parts) {
            *req.uri_mut() = u;
        }
    }
    req
}

pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Current wall-clock time in epoch milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(FeedMode::from_code(2), Some(FeedMode::Quote));
        assert_eq!(FeedMode::from_code(9), None);
        assert!(FeedMode::Depth > FeedMode::Quote);
        assert_eq!(FeedMode::Depth.code(), 3);
    }

    #[test]
    fn empty_path_becomes_root_and_keeps_the_query() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let r = normalize_request(
            "wss://ws.kite.trade?api_key=k&access_token=t"
                .into_client_request()
                .unwrap(),
        );
        assert_eq!(
            r.uri().path_and_query().unwrap().as_str(),
            "/?api_key=k&access_token=t"
        );
        assert_eq!(r.uri().query(), Some("api_key=k&access_token=t"));
        assert_eq!(r.uri().host(), Some("ws.kite.trade"));
        let r = normalize_request("ws://127.0.0.1:9/feed?x=1".into_client_request().unwrap());
        assert_eq!(r.uri().to_string(), "ws://127.0.0.1:9/feed?x=1");
    }

    #[test]
    fn change_is_derived_from_close() {
        let mut t = NormalizedTick {
            ltp: 1424.0,
            close: 1418.0,
            ..Default::default()
        };
        t.derive_change();
        assert_eq!(t.change, 6.0);
        assert_eq!(t.change_percent, 0.42);
    }
}
