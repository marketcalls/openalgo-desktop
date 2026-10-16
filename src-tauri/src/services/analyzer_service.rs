//! Analyzer (sandbox) mode: status, toggle, and the engine lifecycle.
//!
//! Turning analyzer mode on starts the sandbox engine (after its catch-up)
//! with live ticks from the market-data feed; turning it off stops it
//! (web `services/analyzer_service.py`).

use super::core::{Reply, UNEXPECTED};
use super::sandbox_feed::FeedTicks;
use crate::error::Result;
use crate::events::Event;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

/// Analyzer status data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerStatus {
    pub analyze_mode: bool,
    pub mode: String,
    pub total_logs: i64,
}

pub struct AnalyzerService;

/// How a caller changes the mode.
#[derive(Debug, Clone, Copy)]
enum ModeChange {
    Toggle,
    Set(bool),
}

impl AnalyzerService {
    /// Current analyzer status.
    pub fn get_status(state: &AppState) -> Result<AnalyzerStatus> {
        let analyze_mode = state.sqlite.get_analyze_mode()?;
        let total_logs = state.logs.count_analyzer_logs().unwrap_or(0);
        Ok(AnalyzerStatus {
            analyze_mode,
            mode: if analyze_mode { "analyze" } else { "live" }.to_string(),
            total_logs,
        })
    }

    /// Flip the mode (the app's Live/Analyze switch). The current mode is
    /// read under the change's lock, so two clicks cannot both flip from
    /// the same value.
    pub async fn toggle_mode(state: &AppState) -> Result<AnalyzerStatus> {
        Self::change_mode(state, ModeChange::Toggle).await
    }

    /// Set the mode (`/api/v1/analyzer/toggle`, MCP, Telegram).
    pub async fn set_mode(state: &AppState, enable: bool) -> Result<AnalyzerStatus> {
        Self::change_mode(state, ModeChange::Set(enable)).await
    }

    /// Every mode change, as one step under the sandbox's mode lock (N-02;
    /// web `services/analyzer_service.py` `apply_analyze_mode`): write, read
    /// back, have the engine follow the mode actually stored (also when the
    /// write failed), and announce that mode (UI-01), so the store, the
    /// engine and every open page end on the same mode whichever path
    /// changed it.
    async fn change_mode(state: &AppState, change: ModeChange) -> Result<AnalyzerStatus> {
        let transition = state.sandbox.begin_mode_change().await;
        let written = match change {
            ModeChange::Toggle => state
                .sqlite
                .get_analyze_mode()
                .and_then(|on| state.sqlite.set_analyze_mode(!on)),
            ModeChange::Set(on) => state.sqlite.set_analyze_mode(on),
        };
        let stored = state.sqlite.get_analyze_mode();
        if let Ok(on) = stored {
            let ticks: Arc<dyn crate::sandbox::TickSource> = FeedTicks::start(state);
            if let Err(e) = transition.follow(on, ticks).await {
                tracing::error!("Sandbox engine transition failed: {}", e.message);
            }
            state
                .bus
                .publish(Event::AnalyzerModeChanged { analyze_mode: on });
        }
        drop(transition);
        written?;
        stored?;
        Self::get_status(state)
    }

    /// At startup: the engine follows the stored mode, on a task owned by
    /// the context.
    pub fn spawn_resume(state: &AppState) {
        let sandbox = state.sandbox.clone();
        let sqlite = state.sqlite.clone();
        let ticks: Arc<dyn crate::sandbox::TickSource> = FeedTicks::start(state);
        state.spawn(async move {
            let transition = sandbox.begin_mode_change().await;
            match sqlite.get_analyze_mode() {
                Ok(on) => {
                    if let Err(e) = transition.follow(on, ticks).await {
                        tracing::error!("Sandbox engine transition failed: {}", e.message);
                    }
                }
                Err(e) => tracing::error!("Analyzer mode could not be read: {}", e),
            }
        });
    }

    /// `POST /api/v1/analyzer`.
    pub fn status_reply(state: &AppState) -> Reply {
        match Self::get_status(state) {
            Ok(s) => Reply::ok(json!({"status": "success", "data": {
                "analyze_mode": s.analyze_mode, "mode": s.mode, "total_logs": s.total_logs,
            }})),
            Err(e) => {
                tracing::error!("Analyzer status failed: {}", e);
                Reply::error(500, UNEXPECTED)
            }
        }
    }

    /// `POST /api/v1/analyzer/toggle`.
    pub async fn toggle_reply(state: &AppState, mode: bool) -> Reply {
        match Self::set_mode(state, mode).await {
            Ok(s) => Reply::ok(json!({"status": "success", "data": {
                "analyze_mode": s.analyze_mode,
                "message": format!("Analyzer mode switched to {}", s.mode),
                "mode": s.mode,
                "total_logs": s.total_logs,
            }})),
            Err(e) => {
                tracing::error!("Analyzer toggle failed: {}", e);
                Reply::error(500, UNEXPECTED)
            }
        }
    }
}
