//! WebSocket event handlers

use super::manager::MarketTick;

/// Handle market tick event
pub fn on_market_tick(tick: &MarketTick) {
    tracing::debug!(
        "Tick: {} {} LTP: {} Vol: {}",
        tick.exchange,
        tick.symbol,
        tick.ltp,
        tick.volume
    );
}

/// Handle WebSocket connection event
pub fn on_connected(broker_id: &str) {
    tracing::info!("WebSocket connected to {}", broker_id);
}

/// Handle WebSocket disconnection event
pub fn on_disconnected(broker_id: &str) {
    tracing::info!("WebSocket disconnected from {}", broker_id);
}

/// Handle WebSocket error event
pub fn on_error(broker_id: &str, error: &str) {
    tracing::error!("WebSocket error ({}): {}", broker_id, error);
}
