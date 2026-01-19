//! Holdings Service
//!
//! Handles holdings retrieval.
//! Called by both Tauri commands and REST API.

use crate::brokers::types::Holding;
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Result of getting holdings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoldingsResult {
    pub success: bool,
    pub holdings: Vec<Holding>,
    pub mode: String,
}

/// Holdings service for business logic
pub struct HoldingsService;

impl HoldingsService {
    /// Get all holdings
    ///
    /// In analyze mode, returns sandbox holdings.
    /// Otherwise, returns live broker holdings.
    pub async fn get_holdings(
        state: &AppState,
        api_key: Option<&str>,
    ) -> Result<HoldingsResult> {
        info!("HoldingsService::get_holdings");

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            return Self::get_sandbox_holdings(state);
        }

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        let holdings = broker.get_holdings(&auth_token).await?;

        Ok(HoldingsResult {
            success: true,
            holdings,
            mode: "live".to_string(),
        })
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

    fn get_sandbox_holdings(state: &AppState) -> Result<HoldingsResult> {
        let sandbox_holdings = state.sqlite.get_sandbox_holdings()?;

        // Convert sandbox holdings to broker Holding type
        let holdings: Vec<Holding> = sandbox_holdings
            .into_iter()
            .map(|sh| {
                let current_value = sh.quantity as f64 * sh.ltp;
                let invested_value = sh.quantity as f64 * sh.average_price;
                let pnl_percentage = if invested_value > 0.0 {
                    (sh.pnl / invested_value) * 100.0
                } else {
                    0.0
                };
                Holding {
                    symbol: sh.symbol,
                    exchange: sh.exchange,
                    isin: None,
                    quantity: sh.quantity,
                    t1_quantity: 0,
                    average_price: sh.average_price,
                    ltp: sh.ltp,
                    close_price: sh.ltp, // Use LTP as close price
                    pnl: sh.pnl,
                    pnl_percentage,
                    current_value,
                }
            })
            .collect();

        Ok(HoldingsResult {
            success: true,
            holdings,
            mode: "analyze".to_string(),
        })
    }
}
