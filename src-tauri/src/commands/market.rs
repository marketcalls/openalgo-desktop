//! Market holidays and timings commands

use crate::db::sqlite::{CreateHolidayRequest, MarketHoliday, MarketTiming, UpdateTimingRequest};
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

// ============================================================================
// Holiday Commands
// ============================================================================

/// Create a market holiday
#[tauri::command]
pub async fn create_market_holiday(
    state: State<'_, AppState>,
    request: CreateHolidayRequest,
) -> Result<MarketHoliday> {
    tracing::info!("Creating market holiday: {}", request.date);
    state.sqlite.create_market_holiday(&request)
}

/// Get market holidays by year
#[tauri::command]
pub async fn get_market_holidays_by_year(
    state: State<'_, AppState>,
    year: i32,
) -> Result<Vec<MarketHoliday>> {
    state.sqlite.get_market_holidays_by_year(year)
}

#[derive(Debug, Deserialize)]
pub struct GetHolidaysByExchangeRequest {
    pub exchange: String,
    pub year: Option<i32>,
}

/// Get market holidays by exchange
#[tauri::command]
pub async fn get_market_holidays_by_exchange(
    state: State<'_, AppState>,
    request: GetHolidaysByExchangeRequest,
) -> Result<Vec<MarketHoliday>> {
    state.sqlite.get_market_holidays_by_exchange(&request.exchange, request.year)
}

#[derive(Debug, Serialize)]
pub struct IsHolidayResponse {
    pub is_holiday: bool,
    pub exchange: String,
    pub date: String,
}

/// Check if a date is a holiday
#[tauri::command]
pub async fn is_market_holiday(
    state: State<'_, AppState>,
    exchange: String,
    date: String,
) -> Result<IsHolidayResponse> {
    let is_holiday = state.sqlite.is_market_holiday(&exchange, &date)?;
    Ok(IsHolidayResponse {
        is_holiday,
        exchange,
        date,
    })
}

#[derive(Debug, Serialize)]
pub struct DeleteHolidayResponse {
    pub success: bool,
    pub id: i64,
}

/// Delete a market holiday
#[tauri::command]
pub async fn delete_market_holiday(
    state: State<'_, AppState>,
    id: i64,
) -> Result<DeleteHolidayResponse> {
    let success = state.sqlite.delete_market_holiday(id)?;
    Ok(DeleteHolidayResponse { success, id })
}

// ============================================================================
// Timing Commands
// ============================================================================

/// Get all market timings
#[tauri::command]
pub async fn get_all_market_timings(state: State<'_, AppState>) -> Result<Vec<MarketTiming>> {
    state.sqlite.get_all_market_timings()
}

/// Get market timing for a specific exchange
#[tauri::command]
pub async fn get_market_timing(
    state: State<'_, AppState>,
    exchange: String,
) -> Result<Option<MarketTiming>> {
    state.sqlite.get_market_timing(&exchange)
}

#[derive(Debug, Deserialize)]
pub struct UpdateMarketTimingRequest {
    pub exchange: String,
    pub pre_open_start: Option<String>,
    pub pre_open_end: Option<String>,
    pub market_open: Option<String>,
    pub market_close: Option<String>,
    pub post_close_end: Option<String>,
}

/// Update market timing
#[tauri::command]
pub async fn update_market_timing(
    state: State<'_, AppState>,
    request: UpdateMarketTimingRequest,
) -> Result<MarketTiming> {
    let update_req = UpdateTimingRequest {
        pre_open_start: request.pre_open_start,
        pre_open_end: request.pre_open_end,
        market_open: request.market_open,
        market_close: request.market_close,
        post_close_end: request.post_close_end,
    };
    state.sqlite.update_market_timing(&request.exchange, &update_req)
}

#[derive(Debug, Serialize)]
pub struct IsMarketOpenResponse {
    pub is_open: bool,
    pub exchange: String,
}

/// Check if market is currently open
#[tauri::command]
pub async fn is_market_open(
    state: State<'_, AppState>,
    exchange: String,
) -> Result<IsMarketOpenResponse> {
    let is_open = state.sqlite.is_market_open(&exchange)?;
    Ok(IsMarketOpenResponse {
        is_open,
        exchange,
    })
}
