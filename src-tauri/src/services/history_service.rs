//! History Service
//!
//! Handles historical data retrieval from DuckDB and broker APIs.
//! Called by both Tauri commands and REST API.

use crate::db::duckdb::models::MarketDataRow;
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Historical candle data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandleData {
    pub timestamp: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
}

/// History result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryResult {
    pub success: bool,
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    pub candles: Vec<CandleData>,
}

/// Supported intervals
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntervalsResult {
    pub intervals: Vec<String>,
}

/// History service for business logic
pub struct HistoryService;

impl HistoryService {
    /// Get historical OHLCV data
    ///
    /// First tries DuckDB cache, then fetches from broker if not available.
    pub async fn get_history(
        state: &AppState,
        symbol: &str,
        exchange: &str,
        interval: &str,
        from_date: &str,
        to_date: &str,
        api_key: Option<&str>,
    ) -> Result<HistoryResult> {
        info!(
            "HistoryService::get_history - {} {} {} {} to {}",
            symbol, exchange, interval, from_date, to_date
        );

        // Try DuckDB cache first
        match state.duckdb.query_market_data(symbol, exchange, interval, from_date, to_date) {
            Ok(rows) if !rows.is_empty() => {
                let candles: Vec<CandleData> = rows
                    .into_iter()
                    .map(|r| CandleData {
                        timestamp: r.timestamp,
                        open: r.open,
                        high: r.high,
                        low: r.low,
                        close: r.close,
                        volume: r.volume,
                    })
                    .collect();

                return Ok(HistoryResult {
                    success: true,
                    symbol: symbol.to_string(),
                    exchange: exchange.to_string(),
                    interval: interval.to_string(),
                    candles,
                });
            }
            _ => {
                // Cache miss - would fetch from broker here
                // For now, return empty result
                info!("Cache miss for {} {} {}", symbol, exchange, interval);
            }
        }

        // TODO: Fetch from broker API if not in cache
        // This would require broker-specific historical data endpoints

        Ok(HistoryResult {
            success: true,
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
            interval: interval.to_string(),
            candles: vec![],
        })
    }

    /// Get supported intervals
    pub fn get_intervals() -> IntervalsResult {
        IntervalsResult {
            intervals: vec![
                "1m".to_string(),
                "3m".to_string(),
                "5m".to_string(),
                "10m".to_string(),
                "15m".to_string(),
                "30m".to_string(),
                "1h".to_string(),
                "2h".to_string(),
                "4h".to_string(),
                "1d".to_string(),
                "1w".to_string(),
                "1M".to_string(),
            ],
        }
    }

    /// Download and cache historical data
    pub async fn download_history(
        state: &AppState,
        symbol: &str,
        exchange: &str,
        interval: &str,
        from_date: &str,
        to_date: &str,
        _api_key: Option<&str>,
    ) -> Result<usize> {
        info!(
            "HistoryService::download_history - {} {} {} {} to {}",
            symbol, exchange, interval, from_date, to_date
        );

        // TODO: Implement broker-specific historical data download
        // For now, return 0 as placeholder

        Ok(0)
    }

    /// Store market data in DuckDB
    pub fn store_market_data(
        state: &AppState,
        symbol: &str,
        exchange: &str,
        interval: &str,
        candles: Vec<CandleData>,
    ) -> Result<usize> {
        let rows: Vec<MarketDataRow> = candles
            .into_iter()
            .map(|c| MarketDataRow {
                timestamp: c.timestamp,
                open: c.open,
                high: c.high,
                low: c.low,
                close: c.close,
                volume: c.volume,
            })
            .collect();

        let count = rows.len();
        state.duckdb.insert_market_data(symbol, exchange, interval, &rows)?;

        Ok(count)
    }
}
