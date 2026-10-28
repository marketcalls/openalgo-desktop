//! The settings a run is built with, read fresh from `ag_setting` (the
//! web's `settings.is_trading_enabled`, `get_system_prompt_override` and
//! the voice and external data switches as `builder.py` reads them).

use super::schema::{MCP_ENABLED, TRADING_ENABLED, VOICE_AGENT_NAME, VOICE_TRADING_ENABLED};
use super::Snapshot;
use crate::agent::contract::policy::{RunPolicy, RunSettings};
use crate::agent::contract::tool::Surface;
use crate::state::AppState;

/// The config lane's [`RunPolicy`], installed at startup.
pub struct SettingsPolicy;

impl RunPolicy for SettingsPolicy {
    fn settings(&self, app: &AppState, surface: Surface) -> RunSettings {
        let mut settings = run_settings(&Snapshot::read(app));
        // A surface with no registered server gets neither the external data
        // tools nor their prompt section, whatever the switch says (the
        // web's `_effective_mcp_enabled`).
        settings.mcp_enabled &= !app.agent.mcp().for_surface(surface).is_empty();
        settings
    }
}

/// A run's settings from one read. The spoken surface may trade only while
/// the master switch is on too (the web's `trading_effective`), so there is
/// one place that stops every order. `mcp_enabled` is the trader's switch
/// (a registered server for the surface is added by [`SettingsPolicy`]).
pub fn run_settings(s: &Snapshot) -> RunSettings {
    let trading = s.flag(TRADING_ENABLED);
    let name = s.text(VOICE_AGENT_NAME).trim();
    RunSettings {
        trading_enabled: trading,
        voice_trading_enabled: trading && s.flag(VOICE_TRADING_ENABLED),
        mcp_enabled: s.flag(MCP_ENABLED),
        system_prompt_override: s.system_prompt_override(),
        voice_agent_name: (!name.is_empty()).then(|| name.to_string()),
    }
}
