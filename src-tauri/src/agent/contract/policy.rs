//! What a run reads from the trader's Agent settings, and the prompt
//! sections other parts add (the web's `settings.py` reads in `builder.py`,
//! and the skills and external data sections).
//!
//! The config lane implements [`RunPolicy`] over `ag_setting`; the skills
//! lane registers a [`SectionSource`] for the SKILLS and EXTERNAL DATA
//! sections. Until they are installed a run gets the safe answer: trading
//! off, no override, no external data, the shipped voice persona.

use crate::agent::contract::tool::{Capabilities, Surface};
use crate::agent::prompts::PromptSection;
use crate::state::AppState;

/// The settings one run is built with, read once when it is built.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunSettings {
    /// The trader's master trading switch (`trading_enabled`). A run gets
    /// order tools only when its request asked and this is on.
    pub trading_enabled: bool,
    /// The spoken surface's own trading switch (`voice_trading_enabled`,
    /// already ANDed with the master switch).
    pub voice_trading_enabled: bool,
    /// The external data switch (`mcp_enabled`), already false when no
    /// server is registered for the surface.
    pub mcp_enabled: bool,
    /// The trader's replacement system prompt (`system_prompt`); blank is
    /// none.
    pub system_prompt_override: Option<String>,
    /// The spoken surface's agent name (`voice_agent_name`).
    pub voice_agent_name: Option<String>,
}

/// Reads the run's settings (implemented by the config lane). Reads the
/// database: callers run it off the async runtime. A read that fails
/// answers the safe settings (trading off), as the web fails closed.
pub trait RunPolicy: Send + Sync {
    fn settings(&self, app: &AppState, surface: Surface) -> RunSettings;
}

/// Before the config lane installs its own: everything off.
pub struct DefaultPolicy;

impl RunPolicy for DefaultPolicy {
    fn settings(&self, _app: &AppState, _surface: Surface) -> RunSettings {
        RunSettings::default()
    }
}

/// Adds prompt sections to a run (the SKILLS section, the EXTERNAL DATA
/// section). A source that cannot build its section returns none and logs:
/// it costs that section, never the run.
pub trait SectionSource: Send + Sync {
    fn sections(&self, app: &AppState, surface: Surface, caps: &Capabilities)
        -> Vec<PromptSection>;
}
