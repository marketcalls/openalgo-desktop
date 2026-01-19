//! Order Service
//!
//! Handles order placement, modification, and cancellation.
//! Called by both Tauri commands (for UI) and REST API (for external tools).

use crate::brokers::types::{ModifyOrderRequest, OrderRequest, OrderResponse};
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

/// Result of placing an order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderResult {
    pub success: bool,
    pub order_id: Option<String>,
    pub message: String,
    pub mode: String, // "live" or "analyze"
}

/// Result of modifying an order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModifyOrderResult {
    pub success: bool,
    pub order_id: String,
    pub message: String,
}

/// Result of cancelling an order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderResult {
    pub success: bool,
    pub order_id: String,
    pub message: String,
}

/// Order service for business logic
pub struct OrderService;

impl OrderService {
    /// Place an order
    ///
    /// Supports both:
    /// - API-based auth (api_key provided) - for REST API calls
    /// - Session-based auth (api_key is None) - for Tauri command calls
    ///
    /// In analyze mode, routes to sandbox instead of live broker.
    pub async fn place_order(
        state: &AppState,
        order: OrderRequest,
        api_key: Option<&str>,
    ) -> Result<PlaceOrderResult> {
        info!("OrderService::place_order - {:?}", order);

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            return Self::place_sandbox_order(state, order, api_key).await;
        }

        // Get broker session - either from API key or current session
        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        // Look up broker-specific symbol and token from cache
        let mut order = order;
        if let Some(symbol_info) = state.get_symbol_by_name(&order.exchange, &order.symbol) {
            if let Some(brsymbol) = symbol_info.brsymbol {
                info!("Resolved broker symbol: {} -> {}", order.symbol, brsymbol);
                order.broker_symbol = Some(brsymbol);
            }
            // Set the exchange token (needed for Angel One)
            info!("Resolved symbol token: {} -> {}", order.symbol, symbol_info.token);
            order.symbol_token = Some(symbol_info.token);
        } else {
            warn!("Symbol not found in cache: {}:{}", order.exchange, order.symbol);
        }

        // Get broker adapter
        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        // Place order via broker
        match broker.place_order(&auth_token, order.clone()).await {
            Ok(response) => {
                // Log the order
                Self::log_order(state, "placeorder", &order, &response, api_key);

                Ok(PlaceOrderResult {
                    success: true,
                    order_id: Some(response.order_id),
                    message: response.message.unwrap_or_else(|| "Order placed successfully".to_string()),
                    mode: "live".to_string(),
                })
            }
            Err(e) => {
                error!("Failed to place order: {}", e);

                // Log the failed order
                Self::log_order_error(state, "placeorder", &order, &e.to_string(), api_key);

                Err(e)
            }
        }
    }

    /// Modify an existing order
    pub async fn modify_order(
        state: &AppState,
        order_id: &str,
        order: ModifyOrderRequest,
        api_key: Option<&str>,
    ) -> Result<ModifyOrderResult> {
        info!("OrderService::modify_order - {} {:?}", order_id, order);

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            // In analyze mode, we don't modify real orders
            return Ok(ModifyOrderResult {
                success: true,
                order_id: order_id.to_string(),
                message: "Order modified (analyze mode)".to_string(),
            });
        }

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        match broker.modify_order(&auth_token, order_id, order).await {
            Ok(response) => Ok(ModifyOrderResult {
                success: true,
                order_id: response.order_id,
                message: response.message.unwrap_or_else(|| "Order modified successfully".to_string()),
            }),
            Err(e) => {
                error!("Failed to modify order: {}", e);
                Err(e)
            }
        }
    }

    /// Cancel an order
    pub async fn cancel_order(
        state: &AppState,
        order_id: &str,
        variety: Option<&str>,
        api_key: Option<&str>,
    ) -> Result<CancelOrderResult> {
        info!("OrderService::cancel_order - {}", order_id);

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            // Try to cancel in sandbox
            match state.sqlite.cancel_sandbox_order(order_id) {
                Ok(true) => {
                    return Ok(CancelOrderResult {
                        success: true,
                        order_id: order_id.to_string(),
                        message: "Order cancelled (analyze mode)".to_string(),
                    });
                }
                Ok(false) => {
                    return Err(AppError::NotFound(format!("Order {} not found in sandbox", order_id)));
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        broker.cancel_order(&auth_token, order_id, variety).await?;

        Ok(CancelOrderResult {
            success: true,
            order_id: order_id.to_string(),
            message: "Order cancelled successfully".to_string(),
        })
    }

    /// Cancel all open orders
    pub async fn cancel_all_orders(
        state: &AppState,
        api_key: Option<&str>,
    ) -> Result<Vec<CancelOrderResult>> {
        info!("OrderService::cancel_all_orders");

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            // In sandbox mode, we could cancel all sandbox orders
            // For now, just return empty - sandbox has its own reset function
            return Ok(vec![]);
        }

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        // Get all open orders
        let orders = broker.get_order_book(&auth_token).await?;

        let mut results = Vec::new();
        for order in orders {
            // Only cancel pending/open orders
            if order.status == "PENDING" || order.status == "OPEN" || order.status == "TRIGGER PENDING" {
                match broker.cancel_order(&auth_token, &order.order_id, None).await {
                    Ok(_) => {
                        results.push(CancelOrderResult {
                            success: true,
                            order_id: order.order_id.clone(),
                            message: "Cancelled".to_string(),
                        });
                    }
                    Err(e) => {
                        results.push(CancelOrderResult {
                            success: false,
                            order_id: order.order_id.clone(),
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

    /// Get authentication token and broker ID
    ///
    /// If api_key is provided (REST API call), validate it and get auth from DB.
    /// Otherwise, use the current broker session (Tauri command call).
    fn get_auth(state: &AppState, api_key: Option<&str>) -> Result<(String, String)> {
        match api_key {
            Some(key) => {
                // REST API call - validate API key and get auth token
                let _api_key_info = state.sqlite.validate_api_key(key, &state.security)?;

                // Get the current broker session (API key holder shares the session)
                let session = state
                    .get_broker_session()
                    .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

                Ok((session.auth_token, session.broker_id))
            }
            None => {
                // Tauri command call - use current session
                let session = state
                    .get_broker_session()
                    .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

                Ok((session.auth_token, session.broker_id))
            }
        }
    }

    /// Place order in sandbox (analyze mode)
    async fn place_sandbox_order(
        state: &AppState,
        order: OrderRequest,
        _api_key: Option<&str>,
    ) -> Result<PlaceOrderResult> {
        info!("Routing to sandbox (analyze mode)");

        let sandbox_order = state.sqlite.place_sandbox_order(
            &order.symbol,
            &order.exchange,
            &order.side,
            order.quantity,
            order.price,
            &order.order_type,
            &order.product,
        )?;

        Ok(PlaceOrderResult {
            success: true,
            order_id: Some(sandbox_order.order_id),
            message: "Order placed in sandbox".to_string(),
            mode: "analyze".to_string(),
        })
    }

    /// Log successful order to order_logs table
    fn log_order(
        state: &AppState,
        action: &str,
        order: &OrderRequest,
        response: &OrderResponse,
        api_key: Option<&str>,
    ) {
        if let Err(e) = state.sqlite.log_order(
            &response.order_id,
            action,
            &order.symbol,
            &order.exchange,
            &order.side,
            order.quantity,
            Some(order.price),
            &order.order_type,
            &order.product,
            "SUCCESS",
            response.message.as_deref(),
            api_key,
        ) {
            warn!("Failed to log order: {}", e);
        }
    }

    /// Log failed order to order_logs table
    fn log_order_error(
        state: &AppState,
        action: &str,
        order: &OrderRequest,
        error: &str,
        api_key: Option<&str>,
    ) {
        if let Err(e) = state.sqlite.log_order(
            "",
            action,
            &order.symbol,
            &order.exchange,
            &order.side,
            order.quantity,
            Some(order.price),
            &order.order_type,
            &order.product,
            "ERROR",
            Some(error),
            api_key,
        ) {
            warn!("Failed to log order error: {}", e);
        }
    }
}
