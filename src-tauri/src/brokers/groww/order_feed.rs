//! Groww order updates from the order subjects of Groww's NATS feed (web
//! `broker/groww/streaming/groww_order_adapter.py`, openalgo#2474), on the
//! shared feed manager as the order socket.
//!
//! * Before every (re)connect, a socket token for a fresh key pair from the
//!   endpoint the market feed uses (`streaming::socket_token`), asked for
//!   as growwapi 1.5.0 asks; a 401 or 403 is an expired login and stops the
//!   order feed until the trader logs in again, anything else is retried
//!   with the manager's capped backoff.
//! * `wss://socket-api.groww.in` with the socket token, the subscription id
//!   and the `nats` subprotocol, which is dropped when the server does not
//!   echo it, as on the market feed.
//! * The first `INFO` of a connection is answered in one frame, as the web
//!   writes it: `CONNECT` (signed over the server's nonce), `SUB` of the
//!   equity subject `stocks/order/updates.apex.<id>` (sid 1) and the F&O
//!   subject `stocks_fo/order/updates.apex.<id>` (sid 2), then `PING`. The
//!   `INFO` the server sends after `CONNECT` is not answered; a server
//!   `PING` gets `PONG`. Groww has no commodity order subject (and takes
//!   no commodity orders).
//! * Each `MSG` body is an `OrderDetailsBroadCastDto` (`order_push`); its
//!   status and quantities make the update. An equity push names the
//!   instrument by ISIN, which the master does not hold, and its prices
//!   have no stated unit, so the update is completed from
//!   `GET /v1/order/detail/{id}` (`complete`, through
//!   `Broker::complete_order_update`) with the symbol, prices, side, type
//!   and product the order book maps. When that read fails, the push's own
//!   side, type, product and contract stay, and the prices 0.
//! * Pings as the web's order sockets send them: every 20 s, answered
//!   within 10 s.

use super::mapping::{
    groww_segment, map_status, oa_exchange, oa_symbol, reverse_order_type, GrowwOrder,
};
use super::order_push::{self, OrderPush};
use super::streaming::{
    connect_frame, next_op, socket_token, FeedEndpoints, Minted, Op, TokenAsk, MAX_PENDING_BYTES,
};
use super::{Category, GrowwCore};
use crate::brokers::common::streaming::{
    ping, BrokerFeed, FeedEvent, FeedSubscription, Message, OrderUpdate, PrepareError, WsRequest,
    WEB_ORDER_PING_INTERVAL, WEB_PING_TIMEOUT,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use crate::security::Secret;
use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// The equity order subject, before the subscription id (growwapi
/// `constants.py`).
pub const EQUITY_ORDER_SUBJECT: &str = "stocks/order/updates.apex.";
/// The F&O order subject, before the subscription id.
pub const FNO_ORDER_SUBJECT: &str = "stocks_fo/order/updates.apex.";

/// What the trader reads when Groww refuses the order feed's socket token.
pub const ORDERS_LOGIN_EXPIRED: &str =
    "Your Groww login has expired, so order updates stopped. Log in to Groww again.";

/// The order adapter's socket-token ask: growwapi 1.5.0's headers, and a
/// subscription id its subjects need (groww_order_adapter.py
/// `_generate_socket_token`).
const ORDERS: TokenAsk = TokenAsk {
    platform_version: "1.5.0",
    expired: ORDERS_LOGIN_EXPIRED,
    needs_subscription: true,
};

pub struct GrowwOrderFeed {
    http: reqwest::Client,
    auth_token: Secret,
    endpoints: FeedEndpoints,
    symbols: SymbolResolver,
    /// Socket token, subscription id and key pair for this connection.
    minted: Option<Minted>,
    /// Ask for the `nats` subprotocol (dropped when not echoed).
    with_protocol: bool,
    /// Bytes of a partial NATS op (bounded by `MAX_PENDING_BYTES`).
    buf: Vec<u8>,
    connect_sent: bool,
}

/// The update a push makes on its own (web `_fields_from` without a
/// detail): status and quantities, and the push's exchange, side, type,
/// product and contract; prices stay 0, the event's defaults.
fn update_from_push(push: &OrderPush, symbols: &SymbolResolver) -> OrderUpdate {
    let order_status = map_status(&push.order_status);
    let exchange = oa_exchange(&push.exchange, &push.segment);
    OrderUpdate {
        orderid: push.groww_order_id.clone(),
        symbol: symbols.oa_symbol_or_raw(&push.contract_id, &exchange),
        action: push.transaction_type.clone(),
        quantity: push.qty,
        pricetype: reverse_order_type(&push.order_type),
        product: push.product.clone(),
        filled_quantity: push.filled_qty,
        pending_quantity: push.remaining_qty,
        rejection_reason: if order_status == "rejected" {
            push.remark.clone()
        } else {
            String::new()
        },
        order_status,
        exchange,
        ..Default::default()
    }
}

/// `update` with what Groww's record of the order says (web `_fields_from`
/// with a detail, mapped as `direct_get_order_book` maps a book row): the
/// symbol, exchange, side, prices, type and product. Status and
/// quantities stay the push's. The product is Groww's as it came, as the
/// web passes it on.
fn with_detail(update: OrderUpdate, detail: &GrowwOrder, symbols: &SymbolResolver) -> OrderUpdate {
    let exchange = oa_exchange(&detail.exchange, &detail.segment);
    OrderUpdate {
        symbol: oa_symbol(symbols, &detail.trading_symbol, &exchange),
        exchange,
        action: detail.transaction_type.clone(),
        price: detail.price,
        trigger_price: detail.trigger_price,
        pricetype: reverse_order_type(&detail.order_type),
        product: detail.product.clone(),
        average_price: detail.average_fill_price,
        ..update
    }
}

/// `update` completed from `GET /v1/order/detail/{id}` (web
/// `_order_detail`), or as it came when the read fails. The read names the
/// push's segment, which the update's exchange gives back: F&O for NFO and
/// BFO, cash otherwise (Groww pushes no other segment's orders).
pub(super) async fn complete(
    core: &GrowwCore,
    auth: &AuthToken,
    update: OrderUpdate,
) -> OrderUpdate {
    let segment = groww_segment(&update.exchange);
    match order_detail(core, auth, &update.orderid, segment).await {
        Some(detail) => with_detail(update, &detail, &core.symbols),
        None => update,
    }
}

/// Groww's record of one order, or `None` when Groww did not give one:
/// a failed read, a refusal, or a payload that is not an order (an empty
/// one included, which the web takes for no detail).
async fn order_detail(
    core: &GrowwCore,
    auth: &AuthToken,
    order_id: &str,
    segment: &str,
) -> Option<GrowwOrder> {
    let path = format!(
        "/v1/order/detail/{}?segment={}",
        urlencoding::encode(order_id),
        segment
    );
    let reply = match core
        .send(Method::GET, &path, auth, None, Category::NonTrading)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(
                "Groww order detail not read; the update keeps the push's fields: {}",
                crate::brokers::common::redact::redact(e)
            );
            return None;
        }
    };
    let payload = reply.payload();
    if !reply.is_success() || payload.as_object().is_none_or(|o| o.is_empty()) {
        return None;
    }
    serde_json::from_value(payload.clone()).ok()
}

impl GrowwOrderFeed {
    pub fn new(
        http: reqwest::Client,
        auth_token: &str,
        endpoints: FeedEndpoints,
        symbols: SymbolResolver,
    ) -> Self {
        Self {
            http,
            auth_token: Secret::new(auth_token),
            endpoints,
            symbols,
            minted: None,
            with_protocol: true,
            buf: Vec::new(),
            connect_sent: false,
        }
    }

    /// Use these credentials for the next handshake (tests drive the NATS
    /// handshake without a socket-token server).
    #[cfg(test)]
    fn set_minted(&mut self, jwt: &str, subscription: &str, key: Option<super::nkeys::KeyPair>) {
        self.minted = Some(Minted {
            jwt: Secret::new(jwt),
            subscription: subscription.to_string(),
            key,
        });
    }

    /// The one frame answering the server's first `INFO` (web
    /// `_send_handshake`): `CONNECT` signed over its nonce, both order
    /// subjects, `PING`.
    fn handshake(&self, info: &Value) -> Option<String> {
        let m = self.minted.as_ref()?;
        let nonce = info.get("nonce").and_then(Value::as_str).unwrap_or("");
        let (nkey, sig) = match (&m.key, nonce.is_empty()) {
            (Some(k), false) => (Some(k.public_key()), Some(k.sign_nonce(nonce))),
            _ => (None, None),
        };
        let mut frame = connect_frame(m.jwt.expose(), nkey.as_deref(), sig.as_deref());
        for (sid, subject) in [EQUITY_ORDER_SUBJECT, FNO_ORDER_SUBJECT].iter().enumerate() {
            frame.push_str(&format!(
                "SUB {}{} {}\r\n",
                subject,
                m.subscription,
                sid + 1
            ));
        }
        frame.push_str("PING\r\n");
        Some(frame)
    }

    /// The update one `MSG` body carries, if any.
    fn on_msg(&self, payload: &[u8]) -> Option<FeedEvent> {
        let Some(push) = order_push::decode(payload) else {
            tracing::debug!("Groww order feed sent a message that is not an order update");
            return None;
        };
        if push.groww_order_id.is_empty() {
            return None;
        }
        Some(FeedEvent::OrderUpdate(update_from_push(
            &push,
            &self.symbols,
        )))
    }

    /// Buffer one frame and handle every complete NATS op in it (web
    /// `_split_frames`).
    fn parse_ops(&mut self, data: &[u8]) -> Vec<FeedEvent> {
        self.buf.extend_from_slice(data);
        if self.buf.len() > MAX_PENDING_BYTES {
            tracing::warn!("Groww order feed sent an oversized frame; dropping it");
            self.buf.clear();
            return Vec::new();
        }
        let buf = std::mem::take(&mut self.buf);
        let mut out = Vec::new();
        let mut used = 0;
        while let Some((op, n)) = next_op(&buf[used..]) {
            used += n;
            match op {
                // Only the first INFO is the handshake: answering the one
                // the server sends after CONNECT gets the connection refused.
                Op::Info(_) if self.connect_sent => {}
                Op::Info(info) => {
                    self.connect_sent = true;
                    match self.handshake(&info) {
                        Some(frame) => out.push(FeedEvent::Reply(Message::Text(frame))),
                        None => {
                            tracing::warn!("Groww order feed has no socket token to connect with")
                        }
                    }
                }
                Op::Ping => out.push(FeedEvent::Reply(Message::Text("PONG\r\n".into()))),
                Op::Pong | Op::Ok => out.push(FeedEvent::Heartbeat),
                Op::Err(m) => tracing::warn!("Groww order feed error: {}", m),
                Op::Msg { payload, .. } => out.extend(self.on_msg(&payload)),
                Op::Other(line) => tracing::debug!("Groww order feed op ignored: {}", line),
            }
        }
        self.buf = buf;
        self.buf.drain(..used);
        out
    }
}

#[async_trait]
impl BrokerFeed for GrowwOrderFeed {
    fn broker(&self) -> &'static str {
        "groww"
    }

    async fn prepare(&mut self) -> std::result::Result<(), PrepareError> {
        if self.auth_token.expose().trim().is_empty() {
            return Err(PrepareError::AuthFailed(ORDERS_LOGIN_EXPIRED.into()));
        }
        self.minted = Some(
            socket_token(
                &self.http,
                &self.endpoints.socket_token_url,
                self.auth_token.expose(),
                &ORDERS,
            )
            .await?,
        );
        self.with_protocol = true;
        Ok(())
    }

    /// The upgrade as the web's order adapter makes it (`get_headers`):
    /// the socket token, the subscription id and the `nats` subprotocol.
    fn ws_request(&self) -> Result<WsRequest> {
        let failed = || {
            tracing::error!("Groww order feed request could not be built");
            AppError::Broker(
                "Groww order updates could not be started. Log in to Groww again.".into(),
            )
        };
        let m = self.minted.as_ref().ok_or_else(failed)?;
        let mut req = self
            .endpoints
            .ws_url
            .as_str()
            .into_client_request()
            .map_err(|_| failed())?;
        let h = req.headers_mut();
        h.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", m.jwt.expose())).map_err(|_| failed())?,
        );
        h.insert(
            "X-Subscription-Id",
            HeaderValue::from_str(&m.subscription).map_err(|_| failed())?,
        );
        if self.with_protocol {
            h.insert("Sec-WebSocket-Protocol", HeaderValue::from_static("nats"));
        }
        Ok(req)
    }

    fn on_connect_failed(&mut self, error: &str) -> bool {
        // The server did not echo the `nats` subprotocol: retry without it.
        if self.with_protocol && error.to_ascii_lowercase().contains("subprotocol") {
            self.with_protocol = false;
            return true;
        }
        false
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.buf.clear();
        self.connect_sent = false;
        Vec::new()
    }

    /// The order subjects are subscribed in the handshake; the feed holds
    /// no instruments.
    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_ops(t.as_bytes()),
            Message::Binary(b) => self.parse_ops(b),
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }

    /// The web's order sockets ping every 20 s, answered within 10 s
    /// (`websocket_proxy/order_adapter.py`; Groww keeps the default).
    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((WEB_ORDER_PING_INTERVAL, ping()))
    }

    fn heartbeat_reply_timeout(&self) -> Option<Duration> {
        Some(WEB_PING_TIMEOUT)
    }
}

#[cfg(test)]
mod tests;
