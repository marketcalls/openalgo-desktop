//! Symbol Service
//!
//! Handles symbol search, lookup, and master contract operations.
//! Called by both Tauri commands and REST API.

use crate::error::{AppError, Result};
use crate::state::{AppState, SymbolInfo};
use serde::{Deserialize, Serialize};
use tracing::info;

/// Symbol search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolSearchResult {
    pub symbol: String,
    pub token: String,
    pub exchange: String,
    pub name: String,
    pub instrument_type: String,
    pub lot_size: i32,
    pub tick_size: f64,
    pub strike: Option<f64>,
    pub expiry: Option<String>,
}

/// Expiry dates result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpiryResult {
    pub success: bool,
    pub expiry_dates: Vec<String>,
}

/// Symbol service for business logic
pub struct SymbolService;

impl SymbolService {
    /// Search symbols by query
    pub fn search_symbols(
        state: &AppState,
        query: &str,
        exchange: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<SymbolSearchResult>> {
        info!("SymbolService::search_symbols - query={}", query);

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
                    tick_size: s.tick_size,
                    strike: None,  // Not available in SymbolInfo
                    expiry: None,  // Not available in SymbolInfo
                }
            })
            .collect();

        Ok(results)
    }

    /// Get symbol info by exchange and symbol name
    pub fn get_symbol_info(
        state: &AppState,
        exchange: &str,
        symbol: &str,
    ) -> Result<SymbolSearchResult> {
        state
            .get_symbol_by_name(exchange, symbol)
            .map(|s| SymbolSearchResult {
                symbol: s.symbol,
                token: s.token,
                exchange: s.exchange,
                name: s.name,
                instrument_type: s.instrument_type,
                lot_size: s.lot_size,
                tick_size: s.tick_size,
                strike: None,
                expiry: None,
            })
            .ok_or_else(|| AppError::NotFound(format!("Symbol not found: {} {}", exchange, symbol)))
    }

    /// Get symbol info by exchange and token
    pub fn get_symbol_by_token(
        state: &AppState,
        exchange: &str,
        token: &str,
    ) -> Result<SymbolSearchResult> {
        state
            .get_symbol_by_token(exchange, token)
            .map(|s| SymbolSearchResult {
                symbol: s.symbol,
                token: s.token,
                exchange: s.exchange,
                name: s.name,
                instrument_type: s.instrument_type,
                lot_size: s.lot_size,
                tick_size: s.tick_size,
                strike: None,
                expiry: None,
            })
            .ok_or_else(|| AppError::NotFound(format!("Token not found: {} {}", exchange, token)))
    }

    /// Get total symbol count
    pub fn get_symbol_count(state: &AppState) -> usize {
        state.symbol_count()
    }

    /// Get all instruments for an exchange
    pub fn get_instruments(
        state: &AppState,
        exchange: Option<&str>,
    ) -> Vec<SymbolSearchResult> {
        state
            .symbol_cache
            .iter()
            .filter(|entry| {
                exchange
                    .map(|e| entry.value().exchange.eq_ignore_ascii_case(e))
                    .unwrap_or(true)
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
                    tick_size: s.tick_size,
                    strike: None,
                    expiry: None,
                }
            })
            .collect()
    }

    /// Get expiry dates for a symbol (for derivatives)
    pub fn get_expiry_dates(
        state: &AppState,
        symbol: &str,
        exchange: &str,
        instrument_type: &str,
    ) -> Result<ExpiryResult> {
        info!(
            "SymbolService::get_expiry_dates - {} {} {}",
            symbol, exchange, instrument_type
        );

        // Filter symbols to find expiries
        let mut expiry_dates: Vec<String> = state
            .symbol_cache
            .iter()
            .filter(|entry| {
                let s = entry.value();
                s.exchange.eq_ignore_ascii_case(exchange)
                    && s.symbol.starts_with(symbol)
                    && s.instrument_type.eq_ignore_ascii_case(instrument_type)
            })
            .filter_map(|entry| {
                // Extract expiry from symbol name (broker-specific parsing)
                // This is a simplified version - actual implementation depends on symbol format
                let s = entry.value();
                Self::extract_expiry_from_symbol(&s.symbol, symbol)
            })
            .collect();

        expiry_dates.sort();
        expiry_dates.dedup();

        Ok(ExpiryResult {
            success: true,
            expiry_dates,
        })
    }

    /// Refresh symbol master from broker
    pub async fn refresh_symbol_master(state: &AppState) -> Result<usize> {
        info!("SymbolService::refresh_symbol_master");

        let session = state
            .get_broker_session()
            .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

        let broker = state
            .brokers
            .get(&session.broker_id)
            .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

        // Download master contract from broker
        let symbols = broker.download_master_contract(&session.auth_token).await?;

        // Convert to SymbolInfo
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

        info!("Loaded {} symbols", count);

        Ok(count)
    }

    // ========================================================================
    // Private Helper Methods
    // ========================================================================

    /// Extract expiry date from symbol name
    /// This is broker-specific and simplified
    fn extract_expiry_from_symbol(full_symbol: &str, base_symbol: &str) -> Option<String> {
        // Remove base symbol to get suffix
        let suffix = full_symbol.strip_prefix(base_symbol)?;

        // Try to parse date patterns like "24JAN", "24FEB", "24D25" etc.
        // This is a simplified implementation
        if suffix.len() >= 5 {
            // Might be in format like "24JAN25" or "24D25"
            Some(suffix[..5].to_string())
        } else {
            None
        }
    }
}
