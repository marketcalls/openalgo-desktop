//! DuckDB data models

use serde::{Deserialize, Serialize};

/// Market data OHLCV row
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDataRow {
    pub timestamp: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
}

/// Watchlist item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchlistItem {
    pub id: i64,
    pub symbol: String,
    pub exchange: String,
    pub name: String,
    pub list_name: String,
    pub order_index: i32,
}

/// Data catalog entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataCatalogEntry {
    pub id: i64,
    pub symbol: String,
    pub exchange: String,
    pub timeframe: String,
    pub from_date: String,
    pub to_date: String,
    pub row_count: i64,
    pub last_updated: String,
}

/// Download job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadJob {
    pub id: i64,
    pub name: String,
    pub status: String,
    pub total_items: i32,
    pub completed_items: i32,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Download job item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadJobItem {
    pub id: i64,
    pub job_id: i64,
    pub symbol: String,
    pub exchange: String,
    pub timeframe: String,
    pub status: String,
    pub error: Option<String>,
}

/// Symbol metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolMetadata {
    pub symbol: String,
    pub exchange: String,
    pub name: String,
    pub sector: Option<String>,
    pub industry: Option<String>,
    pub market_cap: Option<f64>,
}
