//! WebSocket connection manager for real-time market data
//!
//! Supports three broker WebSocket protocols:
//! - Angel One SmartAPI: Little-endian binary with JSON subscribe
//! - Zerodha Kite: Big-endian binary with JSON subscribe
//! - Fyers HSM: Big-endian binary protocol for auth and subscribe

use crate::error::{AppError, Result};
use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// Subscription mode for market data
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SubscriptionMode {
    Ltp = 1,      // Last traded price only
    Quote = 2,    // LTP + OHLC + volume
    SnapQuote = 3, // Quote + best 5 bid/ask (Angel)
    Full = 4,     // Full market depth
}

impl Default for SubscriptionMode {
    fn default() -> Self {
        Self::Quote
    }
}

/// Market tick data emitted to frontend
#[derive(Debug, Clone, Serialize)]
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
    pub bid_qty: i64,
    pub ask_qty: i64,
    pub oi: i64,
    pub timestamp: i64,
    pub change: f64,
    pub change_percent: f64,
}

impl Default for MarketTick {
    fn default() -> Self {
        Self {
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
            bid_qty: 0,
            ask_qty: 0,
            oi: 0,
            timestamp: 0,
            change: 0.0,
            change_percent: 0.0,
        }
    }
}

/// Market depth level
#[derive(Debug, Clone, Serialize)]
pub struct DepthLevel {
    pub price: f64,
    pub quantity: i64,
    pub orders: i32,
}

/// Market depth data
#[derive(Debug, Clone, Serialize)]
pub struct MarketDepth {
    pub token: String,
    pub exchange: String,
    pub buy: Vec<DepthLevel>,
    pub sell: Vec<DepthLevel>,
    pub timestamp: i64,
}

/// WebSocket connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

/// Commands to send to WebSocket task
enum WebSocketCommand {
    Subscribe(Vec<SubscriptionRequest>),
    Unsubscribe(Vec<(String, String)>), // (exchange, token)
    Disconnect,
}

/// Subscription request
#[derive(Debug, Clone)]
pub struct SubscriptionRequest {
    pub exchange: String,
    pub token: String,
    pub mode: SubscriptionMode,
}

/// Token to symbol mapping for reverse lookup
type TokenMap = Arc<RwLock<HashMap<String, (String, String)>>>; // token -> (symbol, exchange)

/// WebSocket manager for handling market data streams
pub struct WebSocketManager {
    app_handle: AppHandle,
    state: RwLock<ConnectionState>,
    subscriptions: RwLock<HashMap<String, SubscriptionMode>>,
    sender: RwLock<Option<mpsc::Sender<WebSocketCommand>>>,
    token_map: TokenMap,
    broker_id: RwLock<Option<String>>,
}

impl WebSocketManager {
    /// Create new WebSocket manager
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            state: RwLock::new(ConnectionState::Disconnected),
            subscriptions: RwLock::new(HashMap::new()),
            sender: RwLock::new(None),
            token_map: Arc::new(RwLock::new(HashMap::new())),
            broker_id: RwLock::new(None),
        }
    }

    /// Connect to broker WebSocket
    pub async fn connect(
        &self,
        broker_id: &str,
        client_id: &str,
        api_key: &str,
        feed_token: &str,
    ) -> Result<()> {
        // Disconnect existing connection first
        if self.is_connected() {
            self.disconnect().await?;
        }

        *self.state.write() = ConnectionState::Connecting;
        *self.broker_id.write() = Some(broker_id.to_string());

        let url = match broker_id {
            "angel" => format!(
                "wss://smartapisocket.angelone.in/smart-stream?clientCode={}&feedToken={}&apiKey={}",
                client_id, feed_token, api_key
            ),
            "zerodha" => format!(
                "wss://ws.kite.trade?api_key={}&access_token={}",
                api_key, feed_token
            ),
            "fyers" => "wss://socket.fyers.in/hsm/v1-5/prod".to_string(),
            _ => return Err(AppError::Broker(format!("Unknown broker: {}", broker_id))),
        };

        info!("Connecting to {} WebSocket...", broker_id);

        // Build request with headers for Angel
        let request = if broker_id == "angel" {
            use tokio_tungstenite::tungstenite::http::Request;
            Request::builder()
                .uri(&url)
                .header("Authorization", format!("Bearer {}", feed_token))
                .header("x-api-key", api_key)
                .header("x-client-code", client_id)
                .header("x-feed-token", feed_token)
                .body(())
                .map_err(|e| AppError::Internal(format!("Failed to build request: {}", e)))?
        } else if broker_id == "fyers" {
            use tokio_tungstenite::tungstenite::http::Request;
            Request::builder()
                .uri(&url)
                .header("Authorization", feed_token)
                .header("User-Agent", "openalgo-desktop/1.0")
                .body(())
                .map_err(|e| AppError::Internal(format!("Failed to build request: {}", e)))?
        } else {
            use tokio_tungstenite::tungstenite::http::Request;
            Request::builder()
                .uri(&url)
                .body(())
                .map_err(|e| AppError::Internal(format!("Failed to build request: {}", e)))?
        };

        let (ws_stream, _) = connect_async(request).await?;
        let (mut write, mut read) = ws_stream.split();

        // For Fyers, send authentication message
        if broker_id == "fyers" {
            let auth_msg = create_fyers_auth_message(feed_token, "openalgo-desktop");
            write.send(Message::Binary(auth_msg)).await?;
            info!("Sent Fyers authentication message");
        }

        let (tx, mut rx) = mpsc::channel::<WebSocketCommand>(100);
        *self.sender.write() = Some(tx);
        *self.state.write() = ConnectionState::Connected;

        let app_handle = self.app_handle.clone();
        let broker = broker_id.to_string();
        let token_map = self.token_map.clone();

        info!("{} WebSocket connected", broker_id);

        // Spawn WebSocket handler task
        tokio::spawn(async move {
            let mut heartbeat_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));

            loop {
                tokio::select! {
                    // Handle incoming messages
                    msg = read.next() => {
                        match msg {
                            Some(Ok(Message::Binary(data))) => {
                                // Parse binary data based on broker protocol
                                let ticks = parse_broker_ticks(&broker, &data, &token_map);
                                for tick in ticks {
                                    if let Err(e) = app_handle.emit("market_tick", &tick) {
                                        warn!("Failed to emit tick: {}", e);
                                    }
                                }
                            }
                            Some(Ok(Message::Text(text))) => {
                                debug!("Received text message: {}", text);
                                // Handle JSON responses (subscription confirmations, etc.)
                            }
                            Some(Ok(Message::Ping(data))) => {
                                debug!("Received ping, sending pong");
                                // Pong is handled automatically by tungstenite
                            }
                            Some(Ok(Message::Pong(_))) => {
                                debug!("Received pong");
                            }
                            Some(Ok(Message::Close(_))) => {
                                info!("WebSocket closed by server");
                                let _ = app_handle.emit("websocket_disconnected", &broker);
                                break;
                            }
                            Some(Err(e)) => {
                                error!("WebSocket error: {}", e);
                                let _ = app_handle.emit("websocket_error", e.to_string());
                                break;
                            }
                            None => {
                                info!("WebSocket stream ended");
                                break;
                            }
                            _ => {}
                        }
                    }

                    // Handle outgoing commands
                    cmd = rx.recv() => {
                        match cmd {
                            Some(WebSocketCommand::Subscribe(requests)) => {
                                let msg = create_subscribe_message(&broker, &requests);
                                if let Err(e) = write.send(msg).await {
                                    error!("Failed to send subscribe: {}", e);
                                }
                            }
                            Some(WebSocketCommand::Unsubscribe(symbols)) => {
                                let msg = create_unsubscribe_message(&broker, &symbols);
                                if let Err(e) = write.send(msg).await {
                                    error!("Failed to send unsubscribe: {}", e);
                                }
                            }
                            Some(WebSocketCommand::Disconnect) => {
                                let _ = write.close().await;
                                break;
                            }
                            None => break,
                        }
                    }

                    // Send heartbeat
                    _ = heartbeat_interval.tick() => {
                        match broker.as_str() {
                            "angel" => {
                                if let Err(e) = write.send(Message::Text("ping".to_string())).await {
                                    warn!("Failed to send heartbeat: {}", e);
                                }
                            }
                            "zerodha" => {
                                // Zerodha uses 1-byte heartbeat
                                if let Err(e) = write.send(Message::Binary(vec![0])).await {
                                    warn!("Failed to send heartbeat: {}", e);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            info!("{} WebSocket task ended", broker);
        });

        Ok(())
    }

    /// Subscribe to symbols
    pub async fn subscribe(&self, requests: Vec<SubscriptionRequest>) -> Result<()> {
        // Update token map
        {
            let mut map = self.token_map.write();
            for req in &requests {
                map.insert(req.token.clone(), (req.token.clone(), req.exchange.clone()));
            }
        }

        // Update subscriptions
        {
            let mut subs = self.subscriptions.write();
            for req in &requests {
                let key = format!("{}:{}", req.exchange, req.token);
                subs.insert(key, req.mode);
            }
        }

        // Clone sender before await to avoid holding lock across await point
        let tx = {
            let sender = self.sender.read();
            sender.clone()
        };

        if let Some(tx) = tx {
            tx.send(WebSocketCommand::Subscribe(requests))
                .await
                .map_err(|e| AppError::Internal(format!("Failed to send subscribe: {}", e)))?;
        } else {
            return Err(AppError::Internal("WebSocket not connected".to_string()));
        }
        Ok(())
    }

    /// Unsubscribe from symbols
    pub async fn unsubscribe(&self, symbols: Vec<(String, String)>) -> Result<()> {
        // Remove from subscriptions
        {
            let mut subs = self.subscriptions.write();
            for (exchange, token) in &symbols {
                let key = format!("{}:{}", exchange, token);
                subs.remove(&key);
            }
        }

        // Clone sender before await to avoid holding lock across await point
        let tx = {
            let sender = self.sender.read();
            sender.clone()
        };

        if let Some(tx) = tx {
            tx.send(WebSocketCommand::Unsubscribe(symbols))
                .await
                .map_err(|e| AppError::Internal(format!("Failed to send unsubscribe: {}", e)))?;
        }
        Ok(())
    }

    /// Disconnect WebSocket
    pub async fn disconnect(&self) -> Result<()> {
        // Clone sender before await to avoid holding lock across await point
        let tx = {
            let sender = self.sender.read();
            sender.clone()
        };

        if let Some(tx) = tx {
            let _ = tx.send(WebSocketCommand::Disconnect).await;
        }
        *self.state.write() = ConnectionState::Disconnected;
        *self.sender.write() = None;
        *self.broker_id.write() = None;
        self.subscriptions.write().clear();
        Ok(())
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        matches!(*self.state.read(), ConnectionState::Connected)
    }

    /// Get current broker
    pub fn get_broker(&self) -> Option<String> {
        self.broker_id.read().clone()
    }

    /// Register token to symbol mapping
    pub fn register_symbol(&self, token: &str, symbol: &str, exchange: &str) {
        let mut map = self.token_map.write();
        map.insert(token.to_string(), (symbol.to_string(), exchange.to_string()));
    }
}

// ============================================================================
// Binary Protocol Parsing
// ============================================================================

/// Parse broker-specific binary tick data
fn parse_broker_ticks(broker: &str, data: &[u8], token_map: &TokenMap) -> Vec<MarketTick> {
    match broker {
        "angel" => parse_angel_ticks(data, token_map),
        "zerodha" => parse_zerodha_ticks(data, token_map),
        "fyers" => parse_fyers_ticks(data, token_map),
        _ => vec![],
    }
}

/// Parse Angel One binary tick (little-endian)
///
/// Structure:
/// - Byte 0: Subscription mode (1=LTP, 2=Quote, 3=SnapQuote)
/// - Byte 1: Exchange type
/// - Bytes 2-26: Token (25 bytes, null-terminated)
/// - Bytes 27-34: Sequence number (int64)
/// - Bytes 35-42: Exchange timestamp (int64)
/// - Bytes 43-50: LTP (int64, in paise)
/// - Quote mode adds OHLCV at bytes 51-122
/// - SnapQuote adds depth at bytes 123+
fn parse_angel_ticks(data: &[u8], token_map: &TokenMap) -> Vec<MarketTick> {
    if data.len() < 51 {
        return vec![];
    }

    let mut cursor = Cursor::new(data);

    let mode = cursor.read_u8().unwrap_or(0);
    let exchange_type = cursor.read_u8().unwrap_or(0);

    // Parse token (25 bytes, null-terminated)
    let mut token_bytes = [0u8; 25];
    if cursor.get_ref().len() >= 27 {
        token_bytes.copy_from_slice(&data[2..27]);
    }
    let token = String::from_utf8_lossy(&token_bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string();

    cursor.set_position(27);
    let _sequence = cursor.read_i64::<LittleEndian>().unwrap_or(0);
    let timestamp = cursor.read_i64::<LittleEndian>().unwrap_or(0);
    let ltp_paise = cursor.read_i64::<LittleEndian>().unwrap_or(0);

    let exchange = match exchange_type {
        1 => "NSE",
        2 => "NFO",
        3 => "BSE",
        4 => "BFO",
        5 => "MCX",
        7 => "NCX",
        13 => "CDS",
        _ => "NSE",
    };

    // Look up symbol from token map
    let (symbol, _) = token_map
        .read()
        .get(&token)
        .cloned()
        .unwrap_or((token.clone(), exchange.to_string()));

    let mut tick = MarketTick {
        symbol,
        exchange: exchange.to_string(),
        token,
        ltp: ltp_paise as f64 / 100.0,
        timestamp,
        ..Default::default()
    };

    // Parse Quote mode fields (mode 2 or 3)
    if mode >= 2 && data.len() >= 123 {
        cursor.set_position(51);
        let _ltq = cursor.read_i64::<LittleEndian>().unwrap_or(0);
        let avg_price = cursor.read_i64::<LittleEndian>().unwrap_or(0);
        let volume = cursor.read_i64::<LittleEndian>().unwrap_or(0);
        let total_buy_qty = cursor.read_f64::<LittleEndian>().unwrap_or(0.0);
        let total_sell_qty = cursor.read_f64::<LittleEndian>().unwrap_or(0.0);
        let open = cursor.read_i64::<LittleEndian>().unwrap_or(0);
        let high = cursor.read_i64::<LittleEndian>().unwrap_or(0);
        let low = cursor.read_i64::<LittleEndian>().unwrap_or(0);
        let close = cursor.read_i64::<LittleEndian>().unwrap_or(0);

        tick.open = open as f64 / 100.0;
        tick.high = high as f64 / 100.0;
        tick.low = low as f64 / 100.0;
        tick.close = close as f64 / 100.0;
        tick.volume = volume;
        tick.bid_qty = total_buy_qty as i64;
        tick.ask_qty = total_sell_qty as i64;

        // Calculate change
        if tick.close > 0.0 {
            tick.change = tick.ltp - tick.close;
            tick.change_percent = (tick.change / tick.close) * 100.0;
        }
    }

    // Parse SnapQuote mode fields (mode 3)
    if mode == 3 && data.len() >= 347 {
        cursor.set_position(131);
        tick.oi = cursor.read_i64::<LittleEndian>().unwrap_or(0);

        // Parse best 5 bid/ask (simplified - just get best bid/ask)
        cursor.set_position(147);
        // Each level: flag(2) + qty(8) + price(8) + orders(2) = 20 bytes
        // First 5 are sell, next 5 are buy
        if data.len() >= 167 {
            let _sell_flag = cursor.read_i16::<LittleEndian>().unwrap_or(0);
            tick.ask_qty = cursor.read_i64::<LittleEndian>().unwrap_or(0);
            tick.ask = cursor.read_i64::<LittleEndian>().unwrap_or(0) as f64 / 100.0;
        }
        if data.len() >= 247 {
            cursor.set_position(247); // 147 + 5*20
            let _buy_flag = cursor.read_i16::<LittleEndian>().unwrap_or(0);
            tick.bid_qty = cursor.read_i64::<LittleEndian>().unwrap_or(0);
            tick.bid = cursor.read_i64::<LittleEndian>().unwrap_or(0) as f64 / 100.0;
        }
    }

    vec![tick]
}

/// Parse Zerodha Kite binary tick (big-endian)
///
/// Structure:
/// - First 2 bytes: Number of packets (Big Endian unsigned short)
/// - Each packet: 2-byte length header + data
/// - LTP mode: 8 bytes (token + ltp)
/// - Quote mode: 44 bytes
/// - Full mode: 184+ bytes with market depth
fn parse_zerodha_ticks(data: &[u8], token_map: &TokenMap) -> Vec<MarketTick> {
    if data.len() < 4 {
        return vec![];
    }

    let mut cursor = Cursor::new(data);
    let num_packets = cursor.read_u16::<BigEndian>().unwrap_or(0) as usize;

    let mut ticks = Vec::with_capacity(num_packets);
    let mut offset = 2usize;

    for _ in 0..num_packets {
        if offset + 2 > data.len() {
            break;
        }

        let packet_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;

        if offset + packet_len > data.len() || packet_len < 8 {
            break;
        }

        let packet = &data[offset..offset + packet_len];
        if let Some(tick) = parse_zerodha_packet(packet, token_map) {
            ticks.push(tick);
        }

        offset += packet_len;
    }

    ticks
}

/// Parse single Zerodha packet
fn parse_zerodha_packet(packet: &[u8], token_map: &TokenMap) -> Option<MarketTick> {
    if packet.len() < 8 {
        return None;
    }

    let mut cursor = Cursor::new(packet);
    let instrument_token = cursor.read_u32::<BigEndian>().unwrap_or(0);
    let ltp_paise = cursor.read_i32::<BigEndian>().unwrap_or(0);

    let token = instrument_token.to_string();

    // Determine exchange from token (Zerodha encodes exchange in token)
    // Token format: exchange_code * 256 + index
    let exchange_code = instrument_token / 256;
    let exchange = match exchange_code % 256 {
        1 => "NSE",
        2 => "NFO",
        3 => "BSE",
        4 => "BFO",
        5 => "MCX",
        6 => "CDS",
        _ => "NSE",
    };

    let (symbol, _) = token_map
        .read()
        .get(&token)
        .cloned()
        .unwrap_or((token.clone(), exchange.to_string()));

    let mut tick = MarketTick {
        symbol,
        exchange: exchange.to_string(),
        token,
        ltp: ltp_paise as f64 / 100.0,
        timestamp: chrono::Utc::now().timestamp_millis(),
        ..Default::default()
    };

    // Quote mode (44 bytes)
    if packet.len() >= 44 {
        cursor.set_position(8);
        let _ltq = cursor.read_i32::<BigEndian>().unwrap_or(0);
        let avg_price = cursor.read_i32::<BigEndian>().unwrap_or(0);
        let volume = cursor.read_i32::<BigEndian>().unwrap_or(0);
        let total_buy_qty = cursor.read_i32::<BigEndian>().unwrap_or(0);
        let total_sell_qty = cursor.read_i32::<BigEndian>().unwrap_or(0);
        let open = cursor.read_i32::<BigEndian>().unwrap_or(0);
        let high = cursor.read_i32::<BigEndian>().unwrap_or(0);
        let low = cursor.read_i32::<BigEndian>().unwrap_or(0);
        let close = cursor.read_i32::<BigEndian>().unwrap_or(0);

        tick.open = open as f64 / 100.0;
        tick.high = high as f64 / 100.0;
        tick.low = low as f64 / 100.0;
        tick.close = close as f64 / 100.0;
        tick.volume = volume as i64;
        tick.bid_qty = total_buy_qty as i64;
        tick.ask_qty = total_sell_qty as i64;

        if tick.close > 0.0 {
            tick.change = tick.ltp - tick.close;
            tick.change_percent = (tick.change / tick.close) * 100.0;
        }
    }

    // Full mode with depth (184+ bytes)
    if packet.len() >= 184 {
        cursor.set_position(44);
        let _ltt = cursor.read_i32::<BigEndian>().unwrap_or(0);
        tick.oi = cursor.read_i32::<BigEndian>().unwrap_or(0) as i64;

        // Parse best bid/ask from depth (offset 64)
        if packet.len() >= 76 {
            cursor.set_position(64);
            // Buy side first level
            tick.bid_qty = cursor.read_i32::<BigEndian>().unwrap_or(0) as i64;
            tick.bid = cursor.read_i32::<BigEndian>().unwrap_or(0) as f64 / 100.0;
            let _buy_orders = cursor.read_i16::<BigEndian>().unwrap_or(0);
            let _padding = cursor.read_i16::<BigEndian>().unwrap_or(0);

            // Sell side starts at 64 + 60 = 124
            cursor.set_position(124);
            tick.ask_qty = cursor.read_i32::<BigEndian>().unwrap_or(0) as i64;
            tick.ask = cursor.read_i32::<BigEndian>().unwrap_or(0) as f64 / 100.0;
        }
    }

    Some(tick)
}

/// Parse Fyers HSM binary tick (big-endian)
///
/// Response types:
/// - Type 1: Authentication response
/// - Type 4: Subscription acknowledgment
/// - Type 6: Data feed (market data)
/// - Type 13: Master data
fn parse_fyers_ticks(data: &[u8], token_map: &TokenMap) -> Vec<MarketTick> {
    if data.len() < 7 {
        return vec![];
    }

    let mut cursor = Cursor::new(data);
    let _data_len = cursor.read_u16::<BigEndian>().unwrap_or(0);

    // Skip to message type (typically at byte 2 or after length)
    let msg_type = data.get(2).copied().unwrap_or(0);

    // We only care about type 6 (data feed)
    if msg_type != 6 {
        return vec![];
    }

    // Skip header (5 bytes reserved)
    cursor.set_position(7);

    let scrip_count = if data.len() >= 9 {
        cursor.read_u16::<BigEndian>().unwrap_or(0) as usize
    } else {
        return vec![];
    };

    let mut ticks = Vec::new();
    let mut offset = 9usize;

    for _ in 0..scrip_count {
        if offset >= data.len() {
            break;
        }

        let data_type = data[offset];
        offset += 1;

        match data_type {
            83 => {
                // Snapshot (0x53 = 'S')
                if let Some((tick, new_offset)) = parse_fyers_snapshot(&data[offset..], token_map) {
                    ticks.push(tick);
                    offset += new_offset;
                } else {
                    break;
                }
            }
            85 => {
                // Update (0x55 = 'U')
                // Updates reference previous snapshots - simplified parsing
                if offset + 3 <= data.len() {
                    let _topic_id = u16::from_be_bytes([data[offset], data[offset + 1]]);
                    let field_count = data[offset + 2] as usize;
                    offset += 3 + (field_count * 4);
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    ticks
}

/// Parse Fyers snapshot data
fn parse_fyers_snapshot(data: &[u8], token_map: &TokenMap) -> Option<(MarketTick, usize)> {
    if data.len() < 10 {
        return None;
    }

    let mut offset = 0usize;

    // Topic ID (2 bytes)
    let _topic_id = u16::from_be_bytes([data[offset], data[offset + 1]]);
    offset += 2;

    // Topic name length and value
    let name_len = data[offset] as usize;
    offset += 1;
    let topic_name = if offset + name_len <= data.len() {
        String::from_utf8_lossy(&data[offset..offset + name_len]).to_string()
    } else {
        return None;
    };
    offset += name_len;

    // Field count
    if offset >= data.len() {
        return None;
    }
    let field_count = data[offset] as usize;
    offset += 1;

    // Fyers data fields order:
    // ltp, vol_traded_today, last_traded_time, exch_feed_time,
    // bid_size, ask_size, bid_price, ask_price, last_traded_qty,
    // tot_buy_qty, tot_sell_qty, avg_trade_price, OI, low_price,
    // high_price, Yhigh, Ylow, lower_ckt, upper_ckt, open_price,
    // prev_close_price, type, symbol

    let mut fields = vec![0i32; field_count];
    for i in 0..field_count {
        if offset + 4 > data.len() {
            break;
        }
        fields[i] = i32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]);
        offset += 4;
    }

    // Skip 2 bytes
    offset += 2;

    // Multiplier and precision
    let multiplier = if offset + 2 <= data.len() {
        u16::from_be_bytes([data[offset], data[offset + 1]]) as f64
    } else {
        100.0
    };
    offset += 2;

    let _precision = if offset < data.len() {
        data[offset]
    } else {
        2
    };
    offset += 1;

    // Parse string fields: exchange, exchange_token, symbol
    let mut strings = Vec::new();
    for _ in 0..3 {
        if offset >= data.len() {
            break;
        }
        let str_len = data[offset] as usize;
        offset += 1;
        if offset + str_len <= data.len() {
            strings.push(String::from_utf8_lossy(&data[offset..offset + str_len]).to_string());
            offset += str_len;
        }
    }

    let exchange = strings.get(0).cloned().unwrap_or_default();
    let token = strings.get(1).cloned().unwrap_or_default();
    let symbol = strings.get(2).cloned().unwrap_or(topic_name);

    // Divisor for price conversion
    let divisor = if multiplier > 0.0 { multiplier } else { 100.0 };

    let ltp = fields.get(0).copied().unwrap_or(0) as f64 / divisor;
    let close = fields.get(20).copied().unwrap_or(0) as f64 / divisor;

    let tick = MarketTick {
        symbol,
        exchange,
        token,
        ltp,
        open: fields.get(19).copied().unwrap_or(0) as f64 / divisor,
        high: fields.get(14).copied().unwrap_or(0) as f64 / divisor,
        low: fields.get(13).copied().unwrap_or(0) as f64 / divisor,
        close,
        volume: fields.get(1).copied().unwrap_or(0) as i64,
        bid: fields.get(6).copied().unwrap_or(0) as f64 / divisor,
        ask: fields.get(7).copied().unwrap_or(0) as f64 / divisor,
        bid_qty: fields.get(4).copied().unwrap_or(0) as i64,
        ask_qty: fields.get(5).copied().unwrap_or(0) as i64,
        oi: fields.get(12).copied().unwrap_or(0) as i64,
        timestamp: fields.get(3).copied().unwrap_or(0) as i64 * 1000,
        change: if close > 0.0 { ltp - close } else { 0.0 },
        change_percent: if close > 0.0 { ((ltp - close) / close) * 100.0 } else { 0.0 },
    };

    Some((tick, offset))
}

// ============================================================================
// Subscribe/Unsubscribe Message Creation
// ============================================================================

/// Create subscribe message for broker
fn create_subscribe_message(broker: &str, requests: &[SubscriptionRequest]) -> Message {
    match broker {
        "angel" => create_angel_subscribe(requests),
        "zerodha" => create_zerodha_subscribe(requests),
        "fyers" => create_fyers_subscribe(requests),
        _ => Message::Text("{}".to_string()),
    }
}

/// Create unsubscribe message for broker
fn create_unsubscribe_message(broker: &str, symbols: &[(String, String)]) -> Message {
    match broker {
        "angel" => create_angel_unsubscribe(symbols),
        "zerodha" => create_zerodha_unsubscribe(symbols),
        "fyers" => create_fyers_unsubscribe(symbols),
        _ => Message::Text("{}".to_string()),
    }
}

/// Angel One subscribe (JSON)
fn create_angel_subscribe(requests: &[SubscriptionRequest]) -> Message {
    // Group tokens by exchange type
    let mut token_lists: HashMap<u8, Vec<String>> = HashMap::new();

    for req in requests {
        let exchange_type = match req.exchange.to_uppercase().as_str() {
            "NSE" => 1,
            "NFO" => 2,
            "BSE" => 3,
            "BFO" => 4,
            "MCX" => 5,
            "NCX" => 7,
            "CDS" => 13,
            _ => 1,
        };
        token_lists.entry(exchange_type).or_default().push(req.token.clone());
    }

    let token_list: Vec<serde_json::Value> = token_lists
        .into_iter()
        .map(|(exchange_type, tokens)| {
            serde_json::json!({
                "exchangeType": exchange_type,
                "tokens": tokens
            })
        })
        .collect();

    // Use the highest mode requested
    let mode = requests
        .iter()
        .map(|r| r.mode as u8)
        .max()
        .unwrap_or(2);

    let msg = serde_json::json!({
        "correlationID": format!("sub_{}", chrono::Utc::now().timestamp_millis()),
        "action": 1,
        "params": {
            "mode": mode,
            "tokenList": token_list
        }
    });

    Message::Text(msg.to_string())
}

/// Angel One unsubscribe (JSON)
fn create_angel_unsubscribe(symbols: &[(String, String)]) -> Message {
    let mut token_lists: HashMap<u8, Vec<String>> = HashMap::new();

    for (exchange, token) in symbols {
        let exchange_type = match exchange.to_uppercase().as_str() {
            "NSE" => 1,
            "NFO" => 2,
            "BSE" => 3,
            "BFO" => 4,
            "MCX" => 5,
            "NCX" => 7,
            "CDS" => 13,
            _ => 1,
        };
        token_lists.entry(exchange_type).or_default().push(token.clone());
    }

    let token_list: Vec<serde_json::Value> = token_lists
        .into_iter()
        .map(|(exchange_type, tokens)| {
            serde_json::json!({
                "exchangeType": exchange_type,
                "tokens": tokens
            })
        })
        .collect();

    let msg = serde_json::json!({
        "correlationID": format!("unsub_{}", chrono::Utc::now().timestamp_millis()),
        "action": 0,
        "params": {
            "mode": 1,
            "tokenList": token_list
        }
    });

    Message::Text(msg.to_string())
}

/// Zerodha Kite subscribe (JSON)
fn create_zerodha_subscribe(requests: &[SubscriptionRequest]) -> Message {
    let tokens: Vec<u32> = requests
        .iter()
        .filter_map(|r| r.token.parse().ok())
        .collect();

    // First subscribe
    let sub_msg = serde_json::json!({
        "a": "subscribe",
        "v": tokens
    });

    // Then set mode
    let mode = match requests.iter().map(|r| r.mode).max().unwrap_or(SubscriptionMode::Quote) {
        SubscriptionMode::Ltp => "ltp",
        SubscriptionMode::Quote => "quote",
        SubscriptionMode::Full | SubscriptionMode::SnapQuote => "full",
    };

    let mode_msg = serde_json::json!({
        "a": "mode",
        "v": [mode, tokens]
    });

    // Send as combined message (Zerodha accepts batch)
    Message::Text(format!("{}\n{}", sub_msg, mode_msg))
}

/// Zerodha Kite unsubscribe (JSON)
fn create_zerodha_unsubscribe(symbols: &[(String, String)]) -> Message {
    let tokens: Vec<u32> = symbols
        .iter()
        .filter_map(|(_, token)| token.parse().ok())
        .collect();

    let msg = serde_json::json!({
        "a": "unsubscribe",
        "v": tokens
    });

    Message::Text(msg.to_string())
}

/// Fyers HSM authentication message (binary)
fn create_fyers_auth_message(hsm_key: &str, source: &str) -> Vec<u8> {
    let mode = "P"; // Production
    let buffer_size = 18 + hsm_key.len() + source.len();

    let mut buffer = Vec::with_capacity(buffer_size);

    // Data length (buffer_size - 2)
    buffer.extend_from_slice(&((buffer_size - 2) as u16).to_be_bytes());

    // Request type = 1 (authentication)
    buffer.push(1);

    // Field count = 4
    buffer.push(4);

    // Field-1: AuthToken (HSM key)
    buffer.push(1); // Field ID
    buffer.extend_from_slice(&(hsm_key.len() as u16).to_be_bytes());
    buffer.extend_from_slice(hsm_key.as_bytes());

    // Field-2: Mode
    buffer.push(2); // Field ID
    buffer.extend_from_slice(&1u16.to_be_bytes());
    buffer.extend_from_slice(mode.as_bytes());

    // Field-3: Unknown flag
    buffer.push(3); // Field ID
    buffer.extend_from_slice(&1u16.to_be_bytes());
    buffer.push(1);

    // Field-4: Source
    buffer.push(4); // Field ID
    buffer.extend_from_slice(&(source.len() as u16).to_be_bytes());
    buffer.extend_from_slice(source.as_bytes());

    buffer
}

/// Fyers HSM subscribe message (binary)
fn create_fyers_subscribe(requests: &[SubscriptionRequest]) -> Message {
    let channel = 11u8; // Default channel

    // Build scrips data
    let mut scrips_data = Vec::new();
    scrips_data.extend_from_slice(&(requests.len() as u16).to_be_bytes());

    for req in requests {
        // Fyers symbol format: exchange:symbol (e.g., "NSE:RELIANCE-EQ")
        let fyers_symbol = format!("{}:{}", req.exchange, req.token);
        let symbol_bytes = fyers_symbol.as_bytes();
        scrips_data.push(symbol_bytes.len() as u8);
        scrips_data.extend_from_slice(symbol_bytes);
    }

    // Build complete message
    let data_len = 6 + scrips_data.len();
    let mut buffer = Vec::with_capacity(data_len + 2);

    buffer.extend_from_slice(&(data_len as u16).to_be_bytes());
    buffer.push(4); // Request type = 4 (subscription)
    buffer.push(2); // Field count = 2

    // Field-1: Symbols
    buffer.push(1); // Field ID
    buffer.extend_from_slice(&(scrips_data.len() as u16).to_be_bytes());
    buffer.extend_from_slice(&scrips_data);

    // Field-2: Channel
    buffer.push(2); // Field ID
    buffer.extend_from_slice(&1u16.to_be_bytes());
    buffer.push(channel);

    Message::Binary(buffer)
}

/// Fyers HSM unsubscribe message (binary)
fn create_fyers_unsubscribe(symbols: &[(String, String)]) -> Message {
    let channel = 11u8;

    // Build scrips data
    let mut scrips_data = Vec::new();
    scrips_data.extend_from_slice(&(symbols.len() as u16).to_be_bytes());

    for (exchange, token) in symbols {
        let fyers_symbol = format!("{}:{}", exchange, token);
        let symbol_bytes = fyers_symbol.as_bytes();
        scrips_data.push(symbol_bytes.len() as u8);
        scrips_data.extend_from_slice(symbol_bytes);
    }

    // Build complete message (action 5 = unsubscribe)
    let data_len = 6 + scrips_data.len();
    let mut buffer = Vec::with_capacity(data_len + 2);

    buffer.extend_from_slice(&(data_len as u16).to_be_bytes());
    buffer.push(5); // Request type = 5 (unsubscription)
    buffer.push(2); // Field count = 2

    // Field-1: Symbols
    buffer.push(1);
    buffer.extend_from_slice(&(scrips_data.len() as u16).to_be_bytes());
    buffer.extend_from_slice(&scrips_data);

    // Field-2: Channel
    buffer.push(2);
    buffer.extend_from_slice(&1u16.to_be_bytes());
    buffer.push(channel);

    Message::Binary(buffer)
}
