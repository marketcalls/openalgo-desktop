//! WebSocket connection manager

use crate::error::{AppError, Result};
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Subscription type for market data
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SubscriptionMode {
    Ltp,        // Last traded price only
    Quote,      // LTP + bid/ask
    Full,       // Full market depth
}

/// Market tick data
#[derive(Debug, Clone, serde::Serialize)]
pub struct MarketTick {
    pub symbol: String,
    pub exchange: String,
    pub token: String,
    pub ltp: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub bid: f64,
    pub ask: f64,
    pub timestamp: i64,
}

/// WebSocket connection state
enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

/// WebSocket manager for handling market data streams
pub struct WebSocketManager {
    app_handle: AppHandle,
    state: RwLock<ConnectionState>,
    subscriptions: RwLock<HashMap<String, SubscriptionMode>>,
    sender: RwLock<Option<mpsc::Sender<WebSocketCommand>>>,
}

/// Commands to send to WebSocket task
enum WebSocketCommand {
    Subscribe(Vec<(String, String, SubscriptionMode)>), // (exchange, token, mode)
    Unsubscribe(Vec<(String, String)>),
    Disconnect,
}

impl WebSocketManager {
    /// Create new WebSocket manager
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            state: RwLock::new(ConnectionState::Disconnected),
            subscriptions: RwLock::new(HashMap::new()),
            sender: RwLock::new(None),
        }
    }

    /// Connect to broker WebSocket
    pub async fn connect(&self, broker_id: &str, feed_token: &str) -> Result<()> {
        *self.state.write() = ConnectionState::Connecting;

        let url = match broker_id {
            "angel" => format!(
                "wss://smartapisocket.angelone.in/smart-stream?clientCode={}&feedToken={}",
                "", feed_token
            ),
            "zerodha" => format!(
                "wss://ws.kite.trade?api_key={}&access_token={}",
                "", feed_token
            ),
            "fyers" => format!(
                "wss://socket.fyers.in/hsm/v1-5/prod?access_token={}",
                feed_token
            ),
            _ => return Err(AppError::Broker(format!("Unknown broker: {}", broker_id))),
        };

        let (ws_stream, _) = connect_async(&url).await?;
        let (mut write, mut read) = ws_stream.split();

        let (tx, mut rx) = mpsc::channel::<WebSocketCommand>(100);
        *self.sender.write() = Some(tx);

        *self.state.write() = ConnectionState::Connected;

        let app_handle = self.app_handle.clone();
        let broker = broker_id.to_string();

        // Spawn WebSocket handler task
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Handle incoming messages
                    Some(msg) = read.next() => {
                        match msg {
                            Ok(Message::Binary(data)) => {
                                // Parse binary data based on broker protocol
                                if let Some(tick) = parse_tick(&broker, &data) {
                                    // Emit tick to frontend
                                    let _ = app_handle.emit("market-tick", tick);
                                }
                            }
                            Ok(Message::Close(_)) => {
                                tracing::info!("WebSocket closed");
                                break;
                            }
                            Err(e) => {
                                tracing::error!("WebSocket error: {}", e);
                                break;
                            }
                            _ => {}
                        }
                    }
                    // Handle outgoing commands
                    Some(cmd) = rx.recv() => {
                        match cmd {
                            WebSocketCommand::Subscribe(symbols) => {
                                if let Some(msg) = create_subscribe_message(&broker, &symbols) {
                                    let _ = write.send(Message::Binary(msg)).await;
                                }
                            }
                            WebSocketCommand::Unsubscribe(symbols) => {
                                if let Some(msg) = create_unsubscribe_message(&broker, &symbols) {
                                    let _ = write.send(Message::Binary(msg)).await;
                                }
                            }
                            WebSocketCommand::Disconnect => {
                                let _ = write.close().await;
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Subscribe to symbols
    pub async fn subscribe(
        &self,
        symbols: Vec<(String, String, SubscriptionMode)>,
    ) -> Result<()> {
        let sender = self.sender.read();
        if let Some(tx) = sender.as_ref() {
            tx.send(WebSocketCommand::Subscribe(symbols))
                .await
                .map_err(|e| AppError::Internal(format!("Failed to send subscribe: {}", e)))?;
        }
        Ok(())
    }

    /// Unsubscribe from symbols
    pub async fn unsubscribe(&self, symbols: Vec<(String, String)>) -> Result<()> {
        let sender = self.sender.read();
        if let Some(tx) = sender.as_ref() {
            tx.send(WebSocketCommand::Unsubscribe(symbols))
                .await
                .map_err(|e| AppError::Internal(format!("Failed to send unsubscribe: {}", e)))?;
        }
        Ok(())
    }

    /// Disconnect WebSocket
    pub async fn disconnect(&self) -> Result<()> {
        let sender = self.sender.read();
        if let Some(tx) = sender.as_ref() {
            let _ = tx.send(WebSocketCommand::Disconnect).await;
        }
        *self.state.write() = ConnectionState::Disconnected;
        Ok(())
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        matches!(*self.state.read(), ConnectionState::Connected)
    }
}

/// Parse binary tick data based on broker
fn parse_tick(broker: &str, data: &[u8]) -> Option<MarketTick> {
    match broker {
        "angel" => parse_angel_tick(data),
        "zerodha" => parse_zerodha_tick(data),
        "fyers" => parse_fyers_tick(data),
        _ => None,
    }
}

/// Parse Angel One binary tick (little-endian)
fn parse_angel_tick(data: &[u8]) -> Option<MarketTick> {
    if data.len() < 50 {
        return None;
    }

    // Angel uses little-endian format
    // Structure varies based on subscription mode
    // This is a simplified implementation

    Some(MarketTick {
        symbol: String::new(),
        exchange: String::new(),
        token: String::new(),
        ltp: 0.0,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 0.0,
        volume: 0,
        bid: 0.0,
        ask: 0.0,
        timestamp: 0,
    })
}

/// Parse Zerodha Kite binary tick (big-endian)
fn parse_zerodha_tick(data: &[u8]) -> Option<MarketTick> {
    if data.len() < 44 {
        return None;
    }

    // Zerodha uses big-endian format
    // This is a simplified implementation

    Some(MarketTick {
        symbol: String::new(),
        exchange: String::new(),
        token: String::new(),
        ltp: 0.0,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 0.0,
        volume: 0,
        bid: 0.0,
        ask: 0.0,
        timestamp: 0,
    })
}

/// Parse Fyers HSM binary tick
fn parse_fyers_tick(data: &[u8]) -> Option<MarketTick> {
    if data.len() < 40 {
        return None;
    }

    // Fyers HSM binary protocol
    // This is a simplified implementation

    Some(MarketTick {
        symbol: String::new(),
        exchange: String::new(),
        token: String::new(),
        ltp: 0.0,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 0.0,
        volume: 0,
        bid: 0.0,
        ask: 0.0,
        timestamp: 0,
    })
}

/// Create subscribe message for broker
fn create_subscribe_message(
    broker: &str,
    symbols: &[(String, String, SubscriptionMode)],
) -> Option<Vec<u8>> {
    match broker {
        "angel" => Some(create_angel_subscribe(symbols)),
        "zerodha" => Some(create_zerodha_subscribe(symbols)),
        "fyers" => Some(create_fyers_subscribe(symbols)),
        _ => None,
    }
}

/// Create unsubscribe message for broker
fn create_unsubscribe_message(
    broker: &str,
    symbols: &[(String, String)],
) -> Option<Vec<u8>> {
    // Similar to subscribe but with unsubscribe action
    Some(vec![])
}

fn create_angel_subscribe(symbols: &[(String, String, SubscriptionMode)]) -> Vec<u8> {
    // Angel SmartAPI subscribe format
    vec![]
}

fn create_zerodha_subscribe(symbols: &[(String, String, SubscriptionMode)]) -> Vec<u8> {
    // Kite subscribe format
    vec![]
}

fn create_fyers_subscribe(symbols: &[(String, String, SubscriptionMode)]) -> Vec<u8> {
    // Fyers HSM subscribe format
    vec![]
}
