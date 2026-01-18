//! Quote and market data commands

use crate::brokers::types::{Quote, MarketDepth};
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::Deserialize;
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct QuoteRequest {
    pub exchange: String,
    pub symbol: String,
}

/// Get quote for symbol(s)
#[tauri::command]
pub async fn get_quote(
    state: State<'_, AppState>,
    symbols: Vec<QuoteRequest>,
) -> Result<Vec<Quote>> {
    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    let symbol_pairs: Vec<(String, String)> = symbols
        .into_iter()
        .map(|s| (s.exchange, s.symbol))
        .collect();

    broker.get_quote(&session.auth_token, symbol_pairs).await
}

/// Get market depth for a symbol
#[tauri::command]
pub async fn get_market_depth(
    state: State<'_, AppState>,
    exchange: String,
    symbol: String,
) -> Result<MarketDepth> {
    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    broker.get_market_depth(&session.auth_token, &exchange, &symbol).await
}
