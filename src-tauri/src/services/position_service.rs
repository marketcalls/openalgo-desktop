//! Position Service
//!
//! Handles position retrieval and closing.
//! Called by both Tauri commands and REST API.

use crate::brokers::types::Position;
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

/// Result of getting positions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionResult {
    pub success: bool,
    pub positions: Vec<Position>,
    pub mode: String,
}

/// Result of closing a position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosePositionResult {
    pub success: bool,
    pub order_id: Option<String>,
    pub message: String,
}

/// Position service for business logic
pub struct PositionService;

impl PositionService {
    /// Get all positions
    ///
    /// In analyze mode, returns sandbox positions.
    /// Otherwise, returns live broker positions.
    pub async fn get_positions(
        state: &AppState,
        api_key: Option<&str>,
    ) -> Result<PositionResult> {
        info!("PositionService::get_positions");

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            return Self::get_sandbox_positions(state);
        }

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        let positions = broker.get_positions(&auth_token).await?;

        Ok(PositionResult {
            success: true,
            positions,
            mode: "live".to_string(),
        })
    }

    /// Get open position for a specific symbol
    pub async fn get_open_position(
        state: &AppState,
        exchange: &str,
        symbol: &str,
        product: &str,
        api_key: Option<&str>,
    ) -> Result<Option<Position>> {
        let result = Self::get_positions(state, api_key).await?;

        // Find matching position
        let position = result.positions.into_iter().find(|p| {
            p.symbol.eq_ignore_ascii_case(symbol)
                && p.exchange.eq_ignore_ascii_case(exchange)
                && p.product.eq_ignore_ascii_case(product)
                && p.quantity != 0
        });

        Ok(position)
    }

    /// Close a specific position
    pub async fn close_position(
        state: &AppState,
        exchange: &str,
        symbol: &str,
        product: &str,
        api_key: Option<&str>,
    ) -> Result<ClosePositionResult> {
        info!("PositionService::close_position - {} {} {}", exchange, symbol, product);

        // Get the current position
        let position = Self::get_open_position(state, exchange, symbol, product, api_key).await?;

        let position = position.ok_or_else(|| {
            AppError::NotFound(format!("No open position for {} {}", exchange, symbol))
        })?;

        if position.quantity == 0 {
            return Ok(ClosePositionResult {
                success: true,
                order_id: None,
                message: "Position already closed".to_string(),
            });
        }

        // Determine action based on position quantity
        let (action, qty) = if position.quantity > 0 {
            ("SELL", position.quantity)
        } else {
            ("BUY", position.quantity.abs())
        };

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            // Close in sandbox
            let order = state.sqlite.place_sandbox_order(
                symbol,
                exchange,
                action,
                qty,
                position.ltp,
                "MARKET",
                product,
            )?;

            return Ok(ClosePositionResult {
                success: true,
                order_id: Some(order.order_id),
                message: "Position close order placed (analyze mode)".to_string(),
            });
        }

        // Place closing order
        let order_request = crate::brokers::types::OrderRequest {
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
            side: action.to_string(),
            quantity: qty,
            order_type: "MARKET".to_string(),
            product: product.to_string(),
            price: 0.0,
            trigger_price: None,
            disclosed_quantity: None,
            validity: "DAY".to_string(),
            amo: false,
            broker_symbol: None,  // Set by OrderService from symbol cache
            symbol_token: None,   // Set by OrderService from symbol cache
        };

        let result = crate::services::OrderService::place_order(state, order_request, api_key).await?;

        Ok(ClosePositionResult {
            success: result.success,
            order_id: result.order_id,
            message: result.message,
        })
    }

    /// Close all open positions
    pub async fn close_all_positions(
        state: &AppState,
        api_key: Option<&str>,
    ) -> Result<Vec<ClosePositionResult>> {
        info!("PositionService::close_all_positions");

        let result = Self::get_positions(state, api_key).await?;

        let mut results = Vec::new();

        for position in result.positions {
            if position.quantity != 0 {
                match Self::close_position(
                    state,
                    &position.exchange,
                    &position.symbol,
                    &position.product,
                    api_key,
                ).await {
                    Ok(close_result) => results.push(close_result),
                    Err(e) => {
                        error!("Failed to close position {} {}: {}", position.exchange, position.symbol, e);
                        results.push(ClosePositionResult {
                            success: false,
                            order_id: None,
                            message: e.to_string(),
                        });
                    }
                }
            }
        }

        Ok(results)
    }

    // ========================================================================
    // Private Helper Methods
    // ========================================================================

    fn get_auth(state: &AppState, api_key: Option<&str>) -> Result<(String, String)> {
        match api_key {
            Some(key) => {
                let _api_key_info = state.sqlite.validate_api_key(key, &state.security)?;
                let session = state
                    .get_broker_session()
                    .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;
                Ok((session.auth_token, session.broker_id))
            }
            None => {
                let session = state
                    .get_broker_session()
                    .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;
                Ok((session.auth_token, session.broker_id))
            }
        }
    }

    fn get_sandbox_positions(state: &AppState) -> Result<PositionResult> {
        let sandbox_positions = state.sqlite.get_sandbox_positions()?;

        // Convert sandbox positions to broker Position type
        let positions: Vec<Position> = sandbox_positions
            .into_iter()
            .map(|sp| Position {
                symbol: sp.symbol,
                exchange: sp.exchange,
                product: sp.product,
                quantity: sp.quantity,
                overnight_quantity: 0,
                average_price: sp.average_price,
                ltp: sp.ltp,
                pnl: sp.pnl,
                realized_pnl: 0.0,
                unrealized_pnl: sp.pnl,
                buy_quantity: 0,
                sell_quantity: 0,
                buy_value: 0.0,
                sell_value: 0.0,
            })
            .collect();

        Ok(PositionResult {
            success: true,
            positions,
            mode: "analyze".to_string(),
        })
    }
}
