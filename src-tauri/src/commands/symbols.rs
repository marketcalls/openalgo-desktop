//! Symbol search and master contract commands

use crate::error::{AppError, Result};
use crate::state::{AppState, SymbolInfo};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct SymbolSearchResult {
    pub symbol: String,
    pub token: String,
    pub exchange: String,
    pub name: String,
    pub instrument_type: String,
    pub lot_size: i32,
}

/// Search symbols by query
#[tauri::command]
pub async fn search_symbols(
    state: State<'_, AppState>,
    query: String,
    exchange: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<SymbolSearchResult>> {
    let limit = limit.unwrap_or(50);
    let query_lower = query.to_lowercase();

    let results: Vec<SymbolSearchResult> = state
        .symbol_cache
        .iter()
        .filter(|entry| {
            let symbol = entry.value();
            let matches_query = symbol.symbol.to_lowercase().contains(&query_lower)
                || symbol.name.to_lowercase().contains(&query_lower);

            let matches_exchange = exchange
                .as_ref()
                .map(|e| symbol.exchange.eq_ignore_ascii_case(e))
                .unwrap_or(true);

            matches_query && matches_exchange
        })
        .take(limit)
        .map(|entry| {
            let s = entry.value();
            SymbolSearchResult {
                symbol: s.symbol.clone(),
                token: s.token.clone(),
                exchange: s.exchange.clone(),
                name: s.name.clone(),
                instrument_type: s.instrument_type.clone(),
                lot_size: s.lot_size,
            }
        })
        .collect();

    Ok(results)
}

/// Get detailed symbol info
#[tauri::command]
pub async fn get_symbol_info(
    state: State<'_, AppState>,
    exchange: String,
    symbol: String,
) -> Result<SymbolSearchResult> {
    let key = format!("{}:{}", exchange, symbol);

    state
        .symbol_cache
        .iter()
        .find(|entry| {
            let s = entry.value();
            s.exchange.eq_ignore_ascii_case(&exchange) && s.symbol.eq_ignore_ascii_case(&symbol)
        })
        .map(|entry| {
            let s = entry.value();
            SymbolSearchResult {
                symbol: s.symbol.clone(),
                token: s.token.clone(),
                exchange: s.exchange.clone(),
                name: s.name.clone(),
                instrument_type: s.instrument_type.clone(),
                lot_size: s.lot_size,
            }
        })
        .ok_or_else(|| AppError::NotFound(format!("Symbol not found: {}", key)))
}

/// Refresh symbol master from broker
#[tauri::command]
pub async fn refresh_symbol_master(state: State<'_, AppState>) -> Result<usize> {
    tracing::info!("Refreshing symbol master");

    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    // Download master contract from broker
    let symbols = broker.download_master_contract(&session.auth_token).await?;

    // Convert to SymbolInfo and store in database
    let symbol_infos: Vec<SymbolInfo> = symbols
        .into_iter()
        .map(|s| SymbolInfo {
            symbol: s.symbol,
            token: s.token,
            exchange: s.exchange,
            name: s.name,
            lot_size: s.lot_size,
            tick_size: s.tick_size,
            instrument_type: s.instrument_type,
        })
        .collect();

    let count = symbol_infos.len();

    // Store in database
    state.sqlite.store_symbols(&symbol_infos)?;

    // Update cache
    state.load_symbol_cache(symbol_infos);

    tracing::info!("Loaded {} symbols", count);

    Ok(count)
}
