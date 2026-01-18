//! Historical data (Historify) commands

use crate::db::duckdb::models::MarketDataRow;
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct MarketDataQuery {
    pub symbol: String,
    pub exchange: String,
    pub timeframe: String, // "1m", "5m", "15m", "1h", "1d"
    pub from_date: String, // ISO date
    pub to_date: String,   // ISO date
}

#[derive(Debug, Deserialize)]
pub struct DownloadRequest {
    pub symbol: String,
    pub exchange: String,
    pub timeframe: String,
    pub from_date: String,
    pub to_date: String,
}

#[derive(Debug, Serialize)]
pub struct DownloadResponse {
    pub success: bool,
    pub rows_downloaded: usize,
    pub message: String,
}

/// Get historical market data from DuckDB
#[tauri::command]
pub async fn get_market_data(
    state: State<'_, AppState>,
    query: MarketDataQuery,
) -> Result<Vec<MarketDataRow>> {
    state.duckdb.query_market_data(
        &query.symbol,
        &query.exchange,
        &query.timeframe,
        &query.from_date,
        &query.to_date,
    )
}

/// Download historical data from broker
#[tauri::command]
pub async fn download_historical_data(
    state: State<'_, AppState>,
    request: DownloadRequest,
) -> Result<DownloadResponse> {
    tracing::info!(
        "Downloading historical data for {}:{} {}",
        request.exchange,
        request.symbol,
        request.timeframe
    );

    // This would typically:
    // 1. Get historical data from broker API
    // 2. Store in DuckDB
    // For now, return a placeholder response

    Ok(DownloadResponse {
        success: true,
        rows_downloaded: 0,
        message: "Historical data download not yet implemented".to_string(),
    })
}
