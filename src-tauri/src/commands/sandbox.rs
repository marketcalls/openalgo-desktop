//! Sandbox (paper trading) commands

use crate::db::sqlite::models::{SandboxOrder, SandboxPosition};
use crate::error::Result;
use crate::state::AppState;
use serde::Deserialize;
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
