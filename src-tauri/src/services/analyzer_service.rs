//! Analyzer Service
//!
//! Handles analyze mode (sandbox/paper trading) state management.
//! Called by both Tauri commands and REST API.

use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Analyzer status data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerStatus {
    pub analyze_mode: bool,
    pub mode: String, // "live" or "analyze"
    pub total_logs: i64,
}

/// Analyzer service for business logic
pub struct AnalyzerService;

impl AnalyzerService {
    /// Get current analyzer status
    pub fn get_status(state: &AppState) -> Result<AnalyzerStatus> {
        let analyze_mode = state.sqlite.get_analyze_mode().unwrap_or(false);
        let total_logs = state.sqlite.count_order_logs(None, None).unwrap_or(0);

        Ok(AnalyzerStatus {
            analyze_mode,
            mode: if analyze_mode { "analyze".to_string() } else { "live".to_string() },
            total_logs,
        })
    }

    /// Toggle analyze mode
    pub fn toggle_mode(state: &AppState, enable: bool) -> Result<AnalyzerStatus> {
        info!("AnalyzerService::toggle_mode - enable={}", enable);

        state.sqlite.set_analyze_mode(enable)?;

        Self::get_status(state)
    }

    /// Reset sandbox data (when exiting analyze mode)
    pub fn reset_sandbox(state: &AppState) -> Result<()> {
        info!("AnalyzerService::reset_sandbox");
        state.sqlite.reset_sandbox()
    }
}
