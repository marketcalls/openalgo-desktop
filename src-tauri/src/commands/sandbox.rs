//! Sandbox (paper trading) commands

use crate::db::sqlite::{SandboxFunds, SandboxHolding};
use crate::db::sqlite::models::{SandboxOrder, SandboxPosition};
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct SandboxOrderRequest {
    pub symbol: String,
    pub exchange: String,
    pub side: String,
    pub quantity: i32,
    pub price: f64,
    pub order_type: String,
    pub product: String,
}

/// Get sandbox positions
#[tauri::command]
pub async fn get_sandbox_positions(state: State<'_, AppState>) -> Result<Vec<SandboxPosition>> {
    state.sqlite.get_sandbox_positions()
}

/// Get sandbox orders
#[tauri::command]
pub async fn get_sandbox_orders(state: State<'_, AppState>) -> Result<Vec<SandboxOrder>> {
    state.sqlite.get_sandbox_orders()
}

/// Place a sandbox order
#[tauri::command]
pub async fn place_sandbox_order(
    state: State<'_, AppState>,
    order: SandboxOrderRequest,
) -> Result<SandboxOrder> {
    tracing::info!("Placing sandbox order: {:?}", order);

    state.sqlite.place_sandbox_order(
        &order.symbol,
        &order.exchange,
        &order.side,
        order.quantity,
        order.price,
        &order.order_type,
        &order.product,
    )
}

/// Reset sandbox (clear all sandbox data)
#[tauri::command]
pub async fn reset_sandbox(state: State<'_, AppState>) -> Result<()> {
    tracing::info!("Resetting sandbox");
    state.sqlite.reset_sandbox()
}

/// Get sandbox holdings
#[tauri::command]
pub async fn get_sandbox_holdings(state: State<'_, AppState>) -> Result<Vec<SandboxHolding>> {
    state.sqlite.get_sandbox_holdings()
}

/// Get sandbox funds
#[tauri::command]
pub async fn get_sandbox_funds(state: State<'_, AppState>) -> Result<SandboxFunds> {
    state.sqlite.get_sandbox_funds()
}

#[derive(Debug, Deserialize)]
pub struct UpdateLtpRequest {
    pub exchange: String,
    pub symbol: String,
    pub ltp: f64,
}

/// Update LTP for sandbox position
#[tauri::command]
pub async fn update_sandbox_ltp(
    state: State<'_, AppState>,
    request: UpdateLtpRequest,
) -> Result<()> {
    state.sqlite.update_sandbox_ltp(&request.exchange, &request.symbol, request.ltp)
}

#[derive(Debug, Serialize)]
pub struct CancelOrderResponse {
    pub success: bool,
    pub order_id: String,
}

/// Cancel a sandbox order
#[tauri::command]
pub async fn cancel_sandbox_order(
    state: State<'_, AppState>,
    order_id: String,
) -> Result<CancelOrderResponse> {
    tracing::info!("Cancelling sandbox order: {}", order_id);
    let success = state.sqlite.cancel_sandbox_order(&order_id)?;
    Ok(CancelOrderResponse {
        success,
        order_id,
    })
}
