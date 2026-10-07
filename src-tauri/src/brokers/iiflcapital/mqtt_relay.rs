//! Loopback relay between the shared `WebSocketManager` and IIFL Capital's
//! MQTT 3.1.1 bridge.
//!
//! The manager speaks WebSocket only and `brokers::upstox::relay` needs a
//! WebSocket upstream, so this module follows the same shape with an MQTT
//! upstream: the feed's `ws_request` points the manager at
//! `ws://127.0.0.1:<port>/<random secret>`; for each manager connection the
//! relay prepares fresh credentials (`MqttUpstream::prepare`), opens one MQTT
//! connection with `rumqttc`, and translates both ways:
//!
//! * downstream text `{"op":"sub"|"unsub","topics":[..]}` becomes SUBSCRIBE
//!   (at most [`MAX_TOPICS_PER_PACKET`] filters per packet, QoS 0) or
//!   UNSUBSCRIBE packets;
//! * every PUBLISH becomes one binary frame `[u16 BE topic length][topic
//!   UTF-8][payload]` ([`encode_publish`] / [`decode_publish`]);
//! * CONNACK accepted -> the relay `READY` control frame; refused with "bad
//!   user name or password" / "not authorized" -> the relay `auth_failed`
//!   control frame (the manager stops until the trader logs in again);
//!   anything else closes the socket so the manager backs off and retries.
//!
//! Control frames use the exact text of `brokers::upstox::relay` so
//! `relay::control` parses them.
//!
//! Resources: one loopback listener per relay, its accept loop a single task
//! owned by [`MqttRelay`] and aborted on drop; at most one session at a time,
//! held in a `JoinSet` inside that task. A session's MQTT event loop runs in
//! a task aborted by a drop guard when the session ends or is aborted, so the
//! broker socket is closed on every exit path. Channels are bounded.

use crate::brokers::upstox::relay::READY;
use crate::error::{AppError, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use rumqttc::{
    AsyncClient, ConnectReturnCode, ConnectionError, Event, MqttOptions, Packet, QoS,
    SubscribeFilter, SubscribeReasonCode, TlsConfiguration, Transport,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::Message;

/// web `MAX_TOKENS_PER_SUBSCRIBE`.
pub const MAX_TOPICS_PER_PACKET: usize = 100;
/// web keepalive (seconds).
pub const KEEPALIVE: Duration = Duration::from_secs(20);
/// Budget for preparing credentials plus the MQTT CONNECT/CONNACK.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(25);
const DOWNSTREAM_HANDSHAKE: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// Requests queued to the MQTT event loop (subscribe batches).
const REQUEST_CAPACITY: usize = 256;
/// Events buffered from the MQTT event loop to the session.
const EVENT_CAPACITY: usize = 1024;
/// Largest packet accepted either way.
const MAX_PACKET: usize = 1 << 20;

/// Where the MQTT bridge lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttEndpoint {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

impl MqttEndpoint {
    pub fn tls(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            tls: true,
        }
    }

    /// Plain TCP (tests).
    pub fn plain(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            tls: false,
        }
    }
}

/// Credentials and the topics to subscribe as soon as the bridge accepts.
#[derive(Clone)]
pub struct Prepared {
    pub client_id: String,
    pub username: String,
    pub password: String,
    pub topics: Vec<String>,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("client_id", &self.client_id)
            .field("topics", &self.topics.len())
            .finish_non_exhaustive()
    }
}

/// Outcome of `prepare`.
#[derive(Debug)]
pub enum Prepare {
    Ready(Prepared),
    /// The stored login cannot work (trader-facing message).
    AuthFailed(String),
    /// Transient: the manager backs off and retries.
    Unavailable,
}

/// One MQTT feed (market data or order updates).
#[async_trait]
pub trait MqttUpstream: Send + Sync + 'static {
    fn broker(&self) -> &'static str;
    fn endpoint(&self) -> &MqttEndpoint;
    /// Fresh credentials for one connection (new client id every time).
    async fn prepare(&self) -> Prepare;
}

/// Downstream control text: subscribe.
pub fn sub_frame(topics: &[String]) -> Message {
    Message::Text(serde_json::json!({"op": "sub", "topics": topics}).to_string())
}

/// Downstream control text: unsubscribe.
pub fn unsub_frame(topics: &[String]) -> Message {
    Message::Text(serde_json::json!({"op": "unsub", "topics": topics}).to_string())
}

/// `[u16 BE topic length][topic][payload]`.
pub fn encode_publish(topic: &str, payload: &[u8]) -> Vec<u8> {
    let t = topic.as_bytes();
    let len = t.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(2 + len + payload.len());
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(&t[..len]);
    out.extend_from_slice(payload);
    out
}

/// Inverse of [`encode_publish`].
pub fn decode_publish(frame: &[u8]) -> Option<(&str, &[u8])> {
    let len = u16::from_be_bytes([*frame.first()?, *frame.get(1)?]) as usize;
    let topic = frame.get(2..2 + len)?;
    let topic = std::str::from_utf8(topic).ok()?;
    Some((topic, &frame[2 + len..]))
}

fn auth_failed_frame(message: &str) -> Message {
    // Same text as `brokers::upstox::relay` so `relay::control` reads it.
    Message::Text(format!(
        "{{\"openalgo_relay\":\"auth_failed\",\"message\":{}}}",
        serde_json::Value::String(message.to_string())
    ))
}

/// A ring-backed rustls client config with the webpki roots (no platform
/// trust store, no aws-lc).
pub fn tls_config() -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| AppError::Internal(format!("TLS setup for the IIFL feed failed: {}", e)))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(cfg))
}

fn options(ep: &MqttEndpoint, p: &Prepared) -> Result<MqttOptions> {
    let mut o = MqttOptions::new(p.client_id.clone(), ep.host.clone(), ep.port);
    o.set_keep_alive(KEEPALIVE);
    o.set_clean_session(true);
    o.set_credentials(p.username.clone(), p.password.clone());
    o.set_max_packet_size(MAX_PACKET, MAX_PACKET);
    o.set_request_channel_capacity(REQUEST_CAPACITY);
    if ep.tls {
        o.set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(
            tls_config()?,
        )));
    }
    Ok(o)
}

/// Aborts a task when dropped.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A running relay. Dropping it stops the listener and any live session.
pub struct MqttRelay {
    url: String,
    task: JoinHandle<()>,
}

impl MqttRelay {
    /// Bind `127.0.0.1:0` and start accepting (needs a tokio runtime).
    pub fn start(upstream: Arc<dyn MqttUpstream>) -> Result<Self> {
        let setup = |e: std::io::Error| {
            AppError::Internal(format!("Feed relay could not open a local port: {}", e))
        };
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(setup)?;
        std_listener.set_nonblocking(true).map_err(setup)?;
        let port = std_listener.local_addr().map_err(setup)?.port();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| AppError::Internal("Feed relay needs the async runtime".into()))?;
        let listener = {
            let _guard = handle.enter();
            tokio::net::TcpListener::from_std(std_listener).map_err(setup)?
        };
        let path = format!("/{}", hex::encode(rand::random::<[u8; 16]>()));
        let task = handle.spawn(accept_loop(listener, path.clone(), upstream));
        Ok(Self {
            url: format!("ws://127.0.0.1:{}{}", port, path),
            task,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }
}

impl Drop for MqttRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Start the relay on first use and hand back its URL.
pub fn ensure_started(
    slot: &parking_lot::Mutex<Option<MqttRelay>>,
    upstream: impl FnOnce() -> Arc<dyn MqttUpstream>,
) -> Result<String> {
    let mut guard = slot.lock();
    if let Some(r) = guard.as_ref() {
        if r.is_running() {
            return Ok(r.url().to_string());
        }
    }
    let r = MqttRelay::start(upstream())?;
    let url = r.url().to_string();
    *guard = Some(r);
    Ok(url)
}

async fn accept_loop(
    listener: tokio::net::TcpListener,
    path: String,
    upstream: Arc<dyn MqttUpstream>,
) {
    let mut sessions: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((tcp, _)) => {
                    sessions.abort_all();
                    while sessions.try_join_next().is_some() {}
                    sessions.spawn(serve(tcp, path.clone(), upstream.clone()));
                }
                Err(e) => {
                    tracing::warn!(broker = upstream.broker(), "Feed relay accept failed: {}", e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            },
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
}

/// Parse a downstream control frame.
fn parse_op(text: &str) -> Option<(bool, Vec<String>)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let sub = match v.get("op")?.as_str()? {
        "sub" => true,
        "unsub" => false,
        _ => return None,
    };
    let topics = v
        .get("topics")?
        .as_array()?
        .iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .filter(|t| !t.is_empty())
        .collect();
    Some((sub, topics))
}

fn subscribe(client: &AsyncClient, topics: &[String]) -> bool {
    for chunk in topics.chunks(MAX_TOPICS_PER_PACKET) {
        let filters: Vec<SubscribeFilter> = chunk
            .iter()
            .map(|t| SubscribeFilter::new(t.clone(), QoS::AtMostOnce))
            .collect();
        if let Err(e) = client.try_subscribe_many(filters) {
            tracing::warn!("IIFL feed subscribe could not be queued: {}", e);
            return false;
        }
    }
    true
}

fn unsubscribe(client: &AsyncClient, topics: &[String]) -> bool {
    for t in topics {
        if let Err(e) = client.try_unsubscribe(t.clone()) {
            tracing::warn!("IIFL feed unsubscribe could not be queued: {}", e);
            return false;
        }
    }
    true
}

fn refused_login(e: &ConnectionError) -> bool {
    matches!(
        e,
        ConnectionError::ConnectionRefused(
            ConnectReturnCode::BadUserNamePassword | ConnectReturnCode::NotAuthorized
        )
    )
}

async fn serve(tcp: TcpStream, path: String, upstream: Arc<dyn MqttUpstream>) {
    let broker = upstream.broker();
    let check =
        move |req: &Request, resp: Response| -> std::result::Result<Response, ErrorResponse> {
            if req.uri().path() == path {
                Ok(resp)
            } else {
                let mut r = ErrorResponse::new(None);
                *r.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
                Err(r)
            }
        };
    let down = match tokio::time::timeout(
        DOWNSTREAM_HANDSHAKE,
        tokio_tungstenite::accept_hdr_async(tcp, check),
    )
    .await
    {
        Ok(Ok(ws)) => ws,
        _ => return,
    };
    let (mut dw, mut dr) = down.split();
    let close_down = |mut dw: futures_util::stream::SplitSink<_, Message>| async move {
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.close()).await;
    };

    let prepared = match tokio::time::timeout(OPEN_TIMEOUT, upstream.prepare()).await {
        Ok(Prepare::Ready(p)) => p,
        Ok(Prepare::AuthFailed(m)) => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.send(auth_failed_frame(&m))).await;
            close_down(dw).await;
            return;
        }
        Ok(Prepare::Unavailable) | Err(_) => {
            close_down(dw).await;
            return;
        }
    };
    let opts = match options(upstream.endpoint(), &prepared) {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(broker, "IIFL feed could not be configured: {}", e.code());
            close_down(dw).await;
            return;
        }
    };
    let (client, mut eventloop) = AsyncClient::new(opts, REQUEST_CAPACITY);
    let (tx, mut events) =
        tokio::sync::mpsc::channel::<std::result::Result<Event, ConnectionError>>(EVENT_CAPACITY);
    // The event loop owns the broker socket; the guard aborts it (closing the
    // socket) whenever this session ends or is aborted.
    let _pump = AbortOnDrop(tokio::spawn(async move {
        loop {
            let ev = eventloop.poll().await;
            let stop = ev.is_err();
            if tx.send(ev).await.is_err() || stop {
                break;
            }
        }
    }));

    // Wait for the CONNACK.
    let connack = tokio::time::timeout(OPEN_TIMEOUT, async {
        while let Some(ev) = events.recv().await {
            match ev {
                Ok(Event::Incoming(Packet::ConnAck(_))) => return Ok(()),
                Ok(_) => continue,
                Err(e) => return Err(Some(e)),
            }
        }
        Err(None)
    })
    .await;
    match connack {
        Ok(Ok(())) => {}
        Ok(Err(Some(e))) if refused_login(&e) => {
            tracing::warn!(broker, "IIFL Capital MQTT bridge refused the login: {}", e);
            let _ = tokio::time::timeout(
                CLOSE_TIMEOUT,
                dw.send(auth_failed_frame(
                    "IIFL Capital refused the live data session. Log in to IIFL Capital again.",
                )),
            )
            .await;
            close_down(dw).await;
            return;
        }
        Ok(Err(e)) => {
            tracing::debug!(
                broker,
                "IIFL Capital MQTT connect failed: {}",
                e.map(|e| e.to_string()).unwrap_or_default()
            );
            close_down(dw).await;
            return;
        }
        Err(_) => {
            tracing::debug!(broker, "IIFL Capital MQTT connect timed out");
            close_down(dw).await;
            return;
        }
    }

    let mut ok = subscribe(&client, &prepared.topics);
    if ok {
        ok = dw.send(Message::Text(READY.into())).await.is_ok();
    }
    while ok {
        tokio::select! {
            m = dr.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    if let Some((sub, topics)) = parse_op(&t) {
                        ok = if sub { subscribe(&client, &topics) } else { unsubscribe(&client, &topics) };
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            ev = events.recv() => match ev {
                Some(Ok(Event::Incoming(Packet::Publish(p)))) => {
                    let frame = encode_publish(&p.topic, &p.payload);
                    if dw.send(Message::Binary(frame)).await.is_err() {
                        ok = false;
                    }
                }
                Some(Ok(Event::Incoming(Packet::SubAck(ack)))) => {
                    let refused = ack
                        .return_codes
                        .iter()
                        .filter(|c| matches!(c, SubscribeReasonCode::Failure))
                        .count();
                    if refused > 0 {
                        tracing::warn!(broker, "IIFL Capital refused {} feed topic(s)", refused);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::debug!(broker, "IIFL Capital MQTT connection ended: {}", e);
                    break;
                }
                None => break,
            }
        }
    }
    let _ = client.try_disconnect();
    // Give the event loop a moment to send DISCONNECT before it is aborted.
    let _ = tokio::time::timeout(Duration::from_millis(200), async {
        while let Some(ev) = events.recv().await {
            if ev.is_err() {
                break;
            }
        }
    })
    .await;
    close_down(dw).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_frames_round_trip() {
        let f = encode_publish("prod/marketfeed/mw/v1/nseeq/2885", &[1, 2, 3]);
        assert_eq!(&f[..2], &[0, 32]);
        let (t, p) = decode_publish(&f).unwrap();
        assert_eq!(t, "prod/marketfeed/mw/v1/nseeq/2885");
        assert_eq!(p, &[1, 2, 3]);
        assert!(decode_publish(&[0]).is_none());
        assert!(decode_publish(&[0, 9, b'a']).is_none());
        let empty = encode_publish("", &[]);
        let (t, p) = decode_publish(&empty).unwrap();
        assert_eq!((t, p.len()), ("", 0));
    }

    #[test]
    fn control_frames_match_the_shared_relay() {
        use crate::brokers::upstox::relay::control;
        let Message::Text(t) = auth_failed_frame("Log in again.") else {
            panic!("text")
        };
        assert_eq!(control(&t), Some(Err("Log in again.".to_string())));
        assert_eq!(control(READY), Some(Ok(())));
        let Message::Text(s) = sub_frame(&["a/b".into()]) else {
            panic!("text")
        };
        assert_eq!(parse_op(&s), Some((true, vec!["a/b".to_string()])));
        let Message::Text(u) = unsub_frame(&["a/b".into()]) else {
            panic!("text")
        };
        assert_eq!(parse_op(&u), Some((false, vec!["a/b".to_string()])));
        assert_eq!(parse_op(r#"{"op":"x","topics":[]}"#), None);
    }

    #[test]
    fn tls_config_builds_with_ring() {
        let c = tls_config().unwrap();
        assert!(c.alpn_protocols.is_empty());
    }
}
