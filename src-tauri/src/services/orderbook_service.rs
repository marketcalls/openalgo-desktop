//! Orderbook and Tradebook Service
//!
//! Handles order book and trade book retrieval.
//! Called by both Tauri commands and REST API.

use crate::brokers::types::Order;
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Result of getting order book
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookResult {
    pub success: bool,
    pub orders: Vec<Order>,
    pub mode: String,
}

/// Result of getting trade book
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradebookResult {
    pub success: bool,
    pub trades: Vec<Order>,
    pub mode: String,
}

/// Order status result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderStatusResult {
    pub success: bool,
    pub order: Option<Order>,
}

/// Orderbook service for business logic
pub struct OrderbookService;

impl OrderbookService {
    /// Get order book
    ///
    /// In analyze mode, returns sandbox orders.
    /// Otherwise, returns live broker orders.
    pub async fn get_orderbook(
        state: &AppState,
        api_key: Option<&str>,
    ) -> Result<OrderbookResult> {
        info!("OrderbookService::get_orderbook");

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            return Self::get_sandbox_orderbook(state);
        }

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        let orders = broker.get_order_book(&auth_token).await?;

        Ok(OrderbookResult {
            success: true,
            orders,
            mode: "live".to_string(),
        })
    }

    /// Get trade book
    ///
    /// In analyze mode, returns sandbox trades.
    /// Otherwise, returns live broker trades.
    pub async fn get_tradebook(
        state: &AppState,
        api_key: Option<&str>,
    ) -> Result<TradebookResult> {
        info!("OrderbookService::get_tradebook");

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            return Self::get_sandbox_tradebook(state);
        }

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        let trades = broker.get_trade_book(&auth_token).await?;

        Ok(TradebookResult {
            success: true,
            trades,
            mode: "live".to_string(),
        })
    }

    /// Get status of a specific order
    pub async fn get_order_status(
        state: &AppState,
        order_id: &str,
        api_key: Option<&str>,
    ) -> Result<OrderStatusResult> {
        info!("OrderbookService::get_order_status - {}", order_id);

        let result = Self::get_orderbook(state, api_key).await?;

        let order = result.orders.into_iter().find(|o| o.order_id == order_id);

        Ok(OrderStatusResult {
            success: true,
            order,
        })
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

    fn get_sandbox_orderbook(state: &AppState) -> Result<OrderbookResult> {
        let sandbox_orders = state.sqlite.get_sandbox_orders()?;

        // Convert sandbox orders to broker Order type
        let orders: Vec<Order> = sandbox_orders
            .into_iter()
            .map(|so| Order {
                order_id: so.order_id,
                exchange_order_id: None,
                symbol: so.symbol,
                exchange: so.exchange,
                side: so.side,
                quantity: so.quantity,
                filled_quantity: so.filled_quantity.unwrap_or(0),
                pending_quantity: so.quantity - so.filled_quantity.unwrap_or(0),
                price: so.price,
                trigger_price: 0.0,
                average_price: so.average_price.unwrap_or(0.0),
                order_type: so.order_type,
                product: so.product,
                status: so.status,
                validity: "DAY".to_string(),
                order_timestamp: so.created_at,
                exchange_timestamp: None,
                rejection_reason: None,
            })
            .collect();

        Ok(OrderbookResult {
            success: true,
            orders,
            mode: "analyze".to_string(),
        })
    }

    fn get_sandbox_tradebook(state: &AppState) -> Result<TradebookResult> {
        // In sandbox, completed orders are trades
        let sandbox_orders = state.sqlite.get_sandbox_orders()?;

        let trades: Vec<Order> = sandbox_orders
            .into_iter()
            .filter(|so| so.status == "complete" || so.status == "COMPLETE" || so.status == "FILLED")
            .map(|so| Order {
                order_id: so.order_id,
                exchange_order_id: None,
                symbol: so.symbol,
                exchange: so.exchange,
                side: so.side,
                quantity: so.filled_quantity.unwrap_or(so.quantity),
                filled_quantity: so.filled_quantity.unwrap_or(so.quantity),
                pending_quantity: 0,
                price: so.price,
                trigger_price: 0.0,
                average_price: so.average_price.unwrap_or(so.price),
                order_type: so.order_type,
                product: so.product,
                status: so.status,
                validity: "DAY".to_string(),
                order_timestamp: so.created_at.clone(),
                exchange_timestamp: Some(so.created_at),
                rejection_reason: None,
            })
            .collect();

        Ok(TradebookResult {
            success: true,
            trades,
            mode: "analyze".to_string(),
        })
    }
}
