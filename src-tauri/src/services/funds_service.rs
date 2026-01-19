//! Funds Service
//!
//! Handles funds/margin retrieval.
//! Called by both Tauri commands and REST API.

use crate::brokers::types::Funds;
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Result of getting funds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundsResult {
    pub success: bool,
    pub funds: Funds,
    pub mode: String,
}

/// Funds service for business logic
pub struct FundsService;

impl FundsService {
    /// Get available funds/margin
    ///
    /// In analyze mode, returns sandbox funds.
    /// Otherwise, returns live broker funds.
    pub async fn get_funds(
        state: &AppState,
        api_key: Option<&str>,
    ) -> Result<FundsResult> {
        info!("FundsService::get_funds");

        // Check if in analyze mode
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);

        if analyze_mode {
            return Self::get_sandbox_funds(state);
        }

        let (auth_token, broker_id) = Self::get_auth(state, api_key)?;

        let broker = state
            .brokers
            .get(&broker_id)
            .ok_or_else(|| AppError::Broker(format!("Broker '{}' not found", broker_id)))?;

        let funds = broker.get_funds(&auth_token).await?;

        Ok(FundsResult {
            success: true,
            funds,
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

    fn get_sandbox_funds(state: &AppState) -> Result<FundsResult> {
        let sandbox_funds = state.sqlite.get_sandbox_funds()?;

        let funds = Funds {
            available_cash: sandbox_funds.available_cash,
            used_margin: sandbox_funds.used_margin,
            total_margin: sandbox_funds.total_value,
            opening_balance: sandbox_funds.total_value,
            payin: 0.0,
            payout: 0.0,
            span: 0.0,
            exposure: 0.0,
            collateral: 0.0,
        };

        Ok(FundsResult {
            success: true,
            funds,
            mode: "analyze".to_string(),
        })
    }
}
