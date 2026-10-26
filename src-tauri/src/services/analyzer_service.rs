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

/// `/api/v1/analyzer` when the mode or the log count cannot be read. The
/// web answers its exception's text; a trader reads this instead.
const STATUS_UNREADABLE: &str = "The analyzer status could not be read just now. Try again.";

/// `/api/v1/analyzer/toggle` in Semi-Auto: the web's refusal, word for word
/// (`services/analyzer_service.py`, 403).
pub(crate) const TOGGLE_SEMI_AUTO: &str = "Operation analyzer/toggle is not allowed in Semi-Auto mode. This operation can only be performed by the client via the UI. This restriction ensures SEBI Research Analyst compliance where mode switching is a client-only decision.";

/// `/api/v1/analyzer/toggle` when the order mode cannot be read: nothing is
/// switched (the web reads a failure as Auto and switches).
const TOGGLE_ORDER_MODE_UNREADABLE: &str = "OpenAlgo could not read whether your orders wait for approval in the Action Center (Semi-Auto), so analyzer mode was not changed. Try again in a moment; if it keeps happening, restart OpenAlgo.";

/// The mode as the answers name it.
fn mode_name(analyze_mode: bool) -> &'static str {
    if analyze_mode {
        "analyze"
    } else {
        "live"
    }
}

impl AnalyzerService {
    /// Current analyzer status. A log count that cannot be read is an
    /// error, as on the web (`get_analyzer_status_with_auth` answers 500),
    /// never a log of zero.
    pub async fn get_status(state: &AppState) -> Result<AnalyzerStatus> {
        let analyze_mode = state.sqlite.get_analyze_mode()?;
        Ok(AnalyzerStatus {
            analyze_mode,
            mode: mode_name(analyze_mode).to_string(),
            total_logs: Self::log_count(state).await?,
        })
    }

    /// The analyzer log's size: a `COUNT(*)` over a log kept forever as on
    /// the web, so it runs on the blocking pool, never on an async worker
    /// that carries the feed.
    async fn log_count(state: &AppState) -> Result<i64> {
        let logs = state.logs.clone();
        tokio::task::spawn_blocking(move || logs.count_analyzer_logs())
            .await
            .map_err(|e| crate::error::AppError::Internal(format!("Blocking task failed: {}", e)))?
    }

    /// Flip the mode (the app's Live/Analyze switch); the mode stored. The
    /// current mode is read under the change's lock, so two clicks cannot
    /// both flip from the same value.
    pub async fn toggle_mode(state: &AppState) -> Result<bool> {
        Self::change_mode(state, ModeChange::Toggle).await
    }

    /// Set the mode (`/api/v1/analyzer/toggle`, MCP, Telegram); the mode
    /// stored.
    pub async fn set_mode(state: &AppState, enable: bool) -> Result<bool> {
        Self::change_mode(state, ModeChange::Set(enable)).await
    }

    /// Every mode change, as one step under the sandbox's mode lock (N-02;
    /// web `services/analyzer_service.py` `apply_analyze_mode`): write, read
    /// back, have the engine follow the mode actually stored (also when the
    /// write failed), and announce that mode (UI-01), so the store, the
    /// engine and every open page end on the same mode whichever path
    /// changed it.
    async fn change_mode(state: &AppState, change: ModeChange) -> Result<bool> {
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
        stored
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
    pub async fn status_reply(state: &AppState) -> Reply {
        match Self::get_status(state).await {
            Ok(s) => Reply::ok(json!({"status": "success", "data": {
                "analyze_mode": s.analyze_mode, "mode": s.mode, "total_logs": s.total_logs,
            }})),
            Err(e) => {
                tracing::error!("Analyzer status failed: {}", e);
                Reply::error(500, STATUS_UNREADABLE)
            }
        }
    }

    /// `POST /api/v1/analyzer/toggle`: the change, then the log count. A
    /// count that cannot be read answers an error after the change, as on
    /// the web (`toggle_analyzer_mode_with_auth`), saying the switch was
    /// made.
    pub async fn toggle_reply(state: &AppState, mode: bool) -> Reply {
        // Semi-Auto: switching modes is the client's own decision, made in
        // the app, never by an API caller (docs-32, web 403).
        match super::order_service::semi_auto(state) {
            Ok(false) => {}
            Ok(true) => return Reply::error(403, TOGGLE_SEMI_AUTO),
            Err(_) => return Reply::error(500, TOGGLE_ORDER_MODE_UNREADABLE),
        }
        let on = match Self::set_mode(state, mode).await {
            Ok(on) => on,
            Err(e) => {
                tracing::error!("Analyzer toggle failed: {}", e);
                return Reply::error(500, UNEXPECTED);
            }
        };
        match Self::log_count(state).await {
            Ok(total_logs) => Reply::ok(json!({"status": "success", "data": {
                "analyze_mode": on,
                "message": format!("Analyzer mode switched to {}", mode_name(on)),
                "mode": mode_name(on),
                "total_logs": total_logs,
            }})),
            Err(e) => {
                tracing::error!("Analyzer log count after the switch failed: {}", e);
                Reply::error(
                    500,
                    format!(
                        "Analyzer mode switched to {}. The analyzer log could not be read just now.",
                        mode_name(on)
                    ),
                )
            }
        }
    }
}
