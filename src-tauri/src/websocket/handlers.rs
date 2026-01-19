//! WebSocket event handlers
//!
//! Provides callback functions for WebSocket lifecycle events.

use super::manager::{MarketDepth, MarketTick};
use tracing::{debug, error, info, warn};

/// Handle market tick event
pub fn on_market_tick(tick: &MarketTick) {
    debug!(
        "Tick: {}:{} LTP: {:.2} Change: {:.2}% Vol: {}",
        tick.exchange, tick.symbol, tick.ltp, tick.change_percent, tick.volume
    );
}

/// Handle market depth event
pub fn on_market_depth(depth: &MarketDepth) {
    debug!(
        "Depth: {}:{} Buy: {} levels, Sell: {} levels",
        depth.exchange,
        depth.token,
        depth.buy.len(),
        depth.sell.len()
    );
}

/// Handle WebSocket connection event
pub fn on_connected(broker_id: &str) {
    info!("WebSocket connected to broker: {}", broker_id);
}

/// Handle WebSocket disconnection event
pub fn on_disconnected(broker_id: &str, reason: Option<&str>) {
    match reason {
        Some(r) => warn!("WebSocket disconnected from {}: {}", broker_id, r),
        None => info!("WebSocket disconnected from {}", broker_id),
    }
}

/// Handle WebSocket error event
pub fn on_error(broker_id: &str, error: &str) {
    error!("WebSocket error ({}): {}", broker_id, error);
}

/// Handle subscription confirmation
pub fn on_subscribed(broker_id: &str, symbols: &[String]) {
    info!(
        "Subscribed to {} symbols on {}: {:?}",
        symbols.len(),
        broker_id,
        symbols.iter().take(5).collect::<Vec<_>>()
    );
}

/// Handle unsubscription confirmation
pub fn on_unsubscribed(broker_id: &str, symbols: &[String]) {
    info!(
        "Unsubscribed from {} symbols on {}",
        symbols.len(),
        broker_id
    );
}

/// Handle reconnection attempt
pub fn on_reconnecting(broker_id: &str, attempt: u32) {
    warn!(
        "Attempting to reconnect to {} WebSocket (attempt {})",
        broker_id, attempt
    );
}

/// Handle authentication success
pub fn on_authenticated(broker_id: &str) {
    info!("WebSocket authenticated with {}", broker_id);
}

/// Handle authentication failure
pub fn on_auth_failed(broker_id: &str, reason: &str) {
    error!("WebSocket authentication failed for {}: {}", broker_id, reason);
}
