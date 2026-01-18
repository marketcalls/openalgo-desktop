//! Order logs commands for audit trail

use crate::db::sqlite::{LogStats, OrderLog};
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct GetLogsRequest {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub broker: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OrderLogsResponse {
    pub status: String,
    pub logs: Vec<OrderLog>,
    pub total: i64,
}

/// Get order logs with pagination and filters
#[tauri::command]
pub async fn get_order_logs(
    state: State<'_, AppState>,
    request: GetLogsRequest,
) -> Result<OrderLogsResponse> {
    let limit = request.limit.unwrap_or(50);
    let offset = request.offset.unwrap_or(0);

    let logs = state.sqlite.get_order_logs(
        limit,
        offset,
        request.broker.as_deref(),
        request.status.as_deref(),
    )?;

    let total = state.sqlite.count_order_logs(
        request.broker.as_deref(),
        request.status.as_deref(),
    )?;

    Ok(OrderLogsResponse {
        status: "success".to_string(),
        logs,
        total,
    })
}

/// Get logs for a specific order
#[tauri::command]
pub async fn get_order_logs_by_order_id(
    state: State<'_, AppState>,
    order_id: String,
) -> Result<Vec<OrderLog>> {
    state.sqlite.get_order_logs_by_order_id(&order_id)
}

/// Get recent order logs for dashboard
#[tauri::command]
pub async fn get_recent_order_logs(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<OrderLog>> {
    state.sqlite.get_recent_order_logs(limit.unwrap_or(10))
}

/// Get order log statistics
#[tauri::command]
pub async fn get_order_log_stats(state: State<'_, AppState>) -> Result<LogStats> {
    state.sqlite.get_order_log_stats()
}

#[derive(Debug, Serialize)]
pub struct ClearLogsResponse {
    pub status: String,
    pub deleted: usize,
    pub message: String,
}

/// Clear old order logs
#[tauri::command]
pub async fn clear_old_order_logs(
    state: State<'_, AppState>,
    days: i32,
) -> Result<ClearLogsResponse> {
    let deleted = state.sqlite.clear_old_order_logs(days)?;

    Ok(ClearLogsResponse {
        status: "success".to_string(),
        deleted,
        message: format!("Cleared {} log entries older than {} days", deleted, days),
    })
}
