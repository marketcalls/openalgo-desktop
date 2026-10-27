//! The setup gate (`GET /agent/api/status`, the web's `agent_status`):
//! whether `/agent` has a model it can run, and the facts the setup screen
//! explains a refusal with.
//!
//! `configured` is true only for an enabled default whose last test passed,
//! the same condition every chat route refuses on. The answer carries a
//! count, never the model list.

use crate::agent::models::store;
use crate::agent::models::success;
use crate::agent::settings::schema::TRADING_ENABLED;
use crate::agent::settings::Snapshot;
use crate::state::AppState;
use serde_json::{json, Value};

/// The gate's answer for the signed-in trader.
pub fn status(app: &AppState) -> Value {
    let read = || -> crate::error::Result<(bool, usize, Option<i64>)> {
        let conn = app.sqlite.conn()?;
        Ok((
            store::is_configured(&conn)?,
            store::list(&conn, true)?.len(),
            store::default_model(&conn)?.map(|r| r.id),
        ))
    };
    let (configured, model_count, default_model_id) = read().unwrap_or_else(|e| {
        tracing::error!("Could not read the Agent configuration state: {}", e);
        (false, 0, None)
    });
    success(json!({
        "configured": configured,
        "model_count": model_count,
        "default_model_id": default_model_id,
        "trading_enabled": Snapshot::read(app).flag(TRADING_ENABLED),
        // The web reports whether its optional agent package is installed;
        // the desktop's Agent is always built in.
        "agent_available": true,
        "has_openalgo_api_key": has_api_key(app),
        // No network and never a device login: safe on every page load.
        "chatgpt_authorised": app.agent.plan().is_authorised(app),
    }))
}

/// Whether the trader has an OpenAlgo API key (the Agent's tools act with
/// it). A key that cannot be read counts as none; the reason is logged by
/// kind only.
fn has_api_key(app: &AppState) -> bool {
    let read = || -> crate::error::Result<bool> {
        let conn = app.sqlite.conn()?;
        Ok(crate::db::sqlite::api_keys::get_plaintext(&conn, &app.security)?.is_some())
    };
    read().unwrap_or_else(|_| {
        tracing::error!("Could not read the OpenAlgo API key for the Agent setup gate");
        false
    })
}
