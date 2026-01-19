//! Quotes Service
//!
//! Handles quote and market depth retrieval.
//! Called by both Tauri commands and REST API.

use crate::brokers::types::{Quote, MarketDepth};
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Result of getting a quote
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteResult {
    pub success: bool,
    pub quotes: Vec<Quote>,
}

/// Result of getting market depth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthResult {
    pub success: bool,
    pub depth: MarketDepth,
}

/// Quotes service for business logic
pub struct QuotesService;

impl QuotesService {
    /// Get quotes for one or more symbols
    pub async fn get_quotes(
        state: &AppState,
        symbols: Vec<(String, String)>, // (exchange, symbol) pairs
        api_key: Option<&str>,
    ) -> Result<QuoteResult> {
        info!("QuotesService::get_quotes - {} symbols", symbols.len());

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        let quotes = broker.get_quote(&auth_token, symbols).await?;

        Ok(QuoteResult {
            success: true,
            quotes,
        })
    }

    /// Get a single quote
    pub async fn get_quote(
        state: &AppState,
        exchange: &str,
        symbol: &str,
        api_key: Option<&str>,
    ) -> Result<Quote> {
        let symbols = vec![(exchange.to_string(), symbol.to_string())];
        let result = Self::get_quotes(state, symbols, api_key).await?;

        result
            .quotes
            .into_iter()
            .next()
            .ok_or_else(|| AppError::NotFound(format!("Quote not found for {} {}", exchange, symbol)))
    }

    /// Get market depth for a symbol
    pub async fn get_market_depth(
        state: &AppState,
        exchange: &str,
        symbol: &str,
        api_key: Option<&str>,
    ) -> Result<DepthResult> {
        info!("QuotesService::get_market_depth - {} {}", exchange, symbol);

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        let depth = broker.get_market_depth(&auth_token, exchange, symbol).await?;

        Ok(DepthResult {
            success: true,
            depth,
        })
    }

    /// Get multi-quotes (multiple symbols at once)
    pub async fn get_multi_quotes(
        state: &AppState,
        symbols: Vec<(String, String)>,
        api_key: Option<&str>,
    ) -> Result<QuoteResult> {
        // Delegate to get_quotes
        Self::get_quotes(state, symbols, api_key).await
    }

    // ========================================================================
    // Private Helper Methods
    // ========================================================================

    fn get_auth(state: &AppState, api_key: Option<&str>) -> Result<(String, String)> {
        match api_key {
            Some(key) => {
                let _api_key_info = state.sqlite.validate_api_key(key, &state.security)?;
                let session = state
                    .get_broker_session()
                    .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;
                Ok((session.auth_token, session.broker_id))
            }
            None => {
                let session = state
                    .get_broker_session()
                    .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;
                Ok((session.auth_token, session.broker_id))
            }
        }
    }
}
