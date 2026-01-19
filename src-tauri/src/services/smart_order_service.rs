//! Smart Order Service
//!
//! Handles smart orders with position sizing and split orders.
//! Called by both Tauri commands and REST API.

use crate::brokers::types::OrderRequest;
use crate::error::Result;
use crate::services::{OrderService, PlaceOrderResult, PositionService};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Smart order request with position sizing
#[derive(Debug, Clone, Deserialize)]
pub struct SmartOrderRequest {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub position_size: i32,
    pub product: String,
    pub pricetype: Option<String>,
    pub price: Option<f64>,
}

/// Split order request
#[derive(Debug, Clone, Deserialize)]
pub struct SplitOrderRequest {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i32,
    pub split_size: i32,
    pub product: String,
    pub pricetype: Option<String>,
    pub price: Option<f64>,
}

/// Result of smart order
#[derive(Debug, Clone, Serialize)]
pub struct SmartOrderResult {
    pub success: bool,
    pub order_id: Option<String>,
    pub action_taken: String, // "BUY", "SELL", or "NONE"
    pub quantity: i32,
    pub message: String,
}

/// Result of split order
#[derive(Debug, Clone, Serialize)]
pub struct SplitOrderResult {
    pub success: bool,
    pub total_quantity: i32,
    pub split_size: i32,
    pub num_orders: i32,
    pub order_ids: Vec<String>,
    pub failed_orders: Vec<String>,
}

/// Smart order service for business logic
pub struct SmartOrderService;

impl SmartOrderService {
    /// Place a smart order with position sizing
    ///
    /// Smart orders calculate the required action based on:
    /// - Current position quantity
    /// - Target position size
    /// - Direction (BUY = long, SELL = short)
    pub async fn place_smart_order(
        state: &AppState,
        req: SmartOrderRequest,
        api_key: Option<&str>,
    ) -> Result<SmartOrderResult> {
        info!(
            "SmartOrderService::place_smart_order - {} {} target={}",
            req.symbol, req.action, req.position_size
        );

        // Get current position
        let current_position = PositionService::get_open_position(
            state,
            &req.exchange,
            &req.symbol,
            &req.product,
            api_key,
        )
        .await?;

        let current_qty = current_position.map(|p| p.quantity).unwrap_or(0);
        let target_size = req.position_size;
        let action = req.action.to_uppercase();

        // Calculate required order
        let (order_action, order_qty) = Self::calculate_smart_action(current_qty, target_size, &action);

        if order_qty == 0 {
            return Ok(SmartOrderResult {
                success: true,
                order_id: None,
                action_taken: "NONE".to_string(),
                quantity: 0,
                message: format!("No action needed. Current position: {}", current_qty),
            });
        }

        // Place the calculated order
        let order_request = OrderRequest {
            symbol: req.symbol.clone(),
            exchange: req.exchange.clone(),
            side: order_action.clone(),
            quantity: order_qty,
            order_type: req.pricetype.clone().unwrap_or_else(|| "MARKET".to_string()),
            product: req.product.clone(),
            price: req.price.unwrap_or(0.0),
            trigger_price: None,
            disclosed_quantity: None,
            validity: "DAY".to_string(),
            amo: false,
        };

        let result = OrderService::place_order(state, order_request, api_key).await?;

        Ok(SmartOrderResult {
            success: result.success,
            order_id: result.order_id,
            action_taken: order_action,
            quantity: order_qty,
            message: result.message,
        })
    }

    /// Place a split order (breaks large order into smaller chunks)
    pub async fn place_split_order(
        state: &AppState,
        req: SplitOrderRequest,
        api_key: Option<&str>,
    ) -> Result<SplitOrderResult> {
        info!(
            "SmartOrderService::place_split_order - {} {} qty={} split={}",
            req.symbol, req.action, req.quantity, req.split_size
        );

        let split_size = if req.split_size > 0 { req.split_size } else { 100 };
        let total_qty = req.quantity;
        let num_orders = (total_qty + split_size - 1) / split_size;

        let mut order_ids = Vec::new();
        let mut failed_orders = Vec::new();
        let mut remaining = total_qty;

        for i in 0..num_orders {
            let qty = std::cmp::min(remaining, split_size);
            remaining -= qty;

            let order_request = OrderRequest {
                symbol: req.symbol.clone(),
                exchange: req.exchange.clone(),
                side: req.action.clone(),
                quantity: qty,
                order_type: req.pricetype.clone().unwrap_or_else(|| "MARKET".to_string()),
                product: req.product.clone(),
                price: req.price.unwrap_or(0.0),
                trigger_price: None,
                disclosed_quantity: None,
                validity: "DAY".to_string(),
                amo: false,
            };

            match OrderService::place_order(state, order_request, api_key).await {
                Ok(result) => {
                    if let Some(order_id) = result.order_id {
                        order_ids.push(order_id);
                    }
                }
                Err(e) => {
                    failed_orders.push(format!("Order {}: {}", i + 1, e));
                }
            }
        }

        Ok(SplitOrderResult {
            success: failed_orders.is_empty(),
            total_quantity: total_qty,
            split_size,
            num_orders,
            order_ids,
            failed_orders,
        })
    }

    /// Place basket order (multiple orders at once)
    pub async fn place_basket_order(
        state: &AppState,
        orders: Vec<OrderRequest>,
        api_key: Option<&str>,
    ) -> Result<Vec<PlaceOrderResult>> {
        info!("SmartOrderService::place_basket_order - {} orders", orders.len());

        let mut results = Vec::new();

        for order in orders {
            match OrderService::place_order(state, order, api_key).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    results.push(PlaceOrderResult {
                        success: false,
                        order_id: None,
                        message: e.to_string(),
                        mode: "live".to_string(),
                    });
                }
            }
        }

        Ok(results)
    }

    // ========================================================================
    // Private Helper Methods
    // ========================================================================

    /// Calculate the action and quantity for a smart order
    ///
    /// Logic:
    /// - If action is BUY and target > current: BUY the difference
    /// - If action is BUY and target < current: SELL the difference
    /// - If action is SELL and target > current: SELL the difference (go short)
    /// - If action is SELL and target < current: BUY to cover
    fn calculate_smart_action(current_qty: i32, target_size: i32, action: &str) -> (String, i32) {
        match action {
            "BUY" => {
                // For BUY: target_size means target LONG position
                let target = target_size;
                if target > current_qty {
                    ("BUY".to_string(), target - current_qty)
                } else if target < current_qty {
                    ("SELL".to_string(), current_qty - target)
                } else {
                    ("NONE".to_string(), 0)
                }
            }
            "SELL" => {
                // For SELL: target_size means target SHORT position (negative)
                let target = -target_size;
                if target < current_qty {
                    ("SELL".to_string(), current_qty - target)
                } else if target > current_qty {
                    ("BUY".to_string(), target - current_qty)
                } else {
                    ("NONE".to_string(), 0)
                }
            }
            _ => ("NONE".to_string(), 0),
        }
    }
}
