//! WebSocket module for real-time market data
//!
//! Supports three broker WebSocket protocols:
//! - Angel One SmartAPI: wss://smartapisocket.angelone.in
//! - Zerodha Kite: wss://ws.kite.trade
//! - Fyers HSM: wss://socket.fyers.in

mod handlers;
mod manager;

pub use handlers::*;
pub use manager::{
    DepthLevel, MarketDepth, MarketTick, SubscriptionMode, SubscriptionRequest, WebSocketManager,
};
