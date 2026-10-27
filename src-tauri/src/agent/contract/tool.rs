//! What every Agent tool implements and what it is given (the web's
//! `tools/base.py` and the `ToolContext` of `tools/__init__.py`).
//!
//! A tool is a name, a description and a JSON schema the model reads, the
//! surfaces it is offered on (through its [`ToolGroup`], the web's
//! toolkit), whether a run pauses for the trader's approval before it runs,
//! and an async [`AgentTool::call`] that gets a [`ToolCtx`] and the model's
//! arguments and answers with a [`ToolOutcome`].
//!
//! Run state lives on the context, never in an argument the model could
//! set: the trader's API key, the run's ids, the trading switch, analyzer
//! mode, the chart the trader has open, the viz side channel and the
//! cancellation token.

use super::viz::VizSink;
use crate::security::Secret;
use crate::state::AppState;
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The most characters one tool result may hand the model (the web's
/// `MAX_JSON_CHARS`). Larger results come back as a well-formed truncation
/// envelope ([`super::json::dumps_capped`]).
pub const MAX_RESULT_CHARS: usize = 12_000;

/// Where a run is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    /// The `/agent` conversation page.
    Chat,
    /// The `/trading` Assistant panel.
    Chart,
    /// A spoken turn (`docs/design/56-voice-agent`); shares chat's tools.
    Voice,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::Chat => "chat",
            Surface::Chart => "chart",
            Surface::Voice => "voice",
        }
    }

    /// The web's `_normalise_surface` plus its check: a known name, case and
    /// spaces ignored; `None` for anything else.
    pub fn parse(raw: &str) -> Option<Surface> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "chat" => Some(Surface::Chat),
            "chart" => Some(Surface::Chart),
            "voice" => Some(Surface::Voice),
            _ => None,
        }
    }
}

/// A set of surfaces (the web's `ALL_SURFACES`, `CHAT_ONLY` and the rest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surfaces {
    chat: bool,
    chart: bool,
    voice: bool,
}

impl Surfaces {
    pub const ALL: Surfaces = Surfaces::of(true, true, true);
    pub const CHAT_ONLY: Surfaces = Surfaces::of(true, false, false);
    pub const CHAT_AND_VOICE: Surfaces = Surfaces::of(true, false, true);
    pub const CHART_ONLY: Surfaces = Surfaces::of(false, true, false);
    pub const CHAT_AND_CHART: Surfaces = Surfaces::of(true, true, false);

    pub const fn of(chat: bool, chart: bool, voice: bool) -> Surfaces {
        Surfaces { chat, chart, voice }
    }

    pub fn contains(self, s: Surface) -> bool {
        match s {
            Surface::Chat => self.chat,
            Surface::Chart => self.chart,
            Surface::Voice => self.voice,
        }
    }
}

/// A run capability a tool group may require (the web's `CAPABILITIES`).
/// Withholding the group is the whole enforcement: a tool that is not
/// built has no schema, so the model cannot call it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// `trading_enabled`: the order tools.
    Trading,
    /// `web_search_enabled`: the web search tools.
    WebSearch,
    /// `mcp_enabled`: the external data (MCP) tools.
    Mcp,
}

/// The capabilities a run was given, each already ANDed with the trader's
/// own setting by the run builder, so a request cannot ask past a switch
/// that is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub trading_enabled: bool,
    pub web_search_enabled: bool,
    pub mcp_enabled: bool,
}

impl Capabilities {
    pub fn has(&self, c: Capability) -> bool {
        match c {
            Capability::Trading => self.trading_enabled,
            Capability::WebSearch => self.web_search_enabled,
            Capability::Mcp => self.mcp_enabled,
        }
    }
}

/// One toolkit: a group of tools offered together (the web's
/// `ToolkitSpec`). Adding a toolkit is one file plus one line in
/// [`crate::agent::tools::TOOLKITS`].
pub struct ToolGroup {
    /// Stable key, unique across the registry (`market`, `orders`, ...).
    pub key: &'static str,
    pub surfaces: Surfaces,
    /// Every capability here must be on for the group to be built.
    pub requires: &'static [Capability],
    /// Lower is offered first; ties break on `key`.
    pub order: u16,
    /// One line for logs and the settings page.
    pub description: &'static str,
    /// Builds the group's tools for one run.
    pub build: fn(&ToolCtx) -> Vec<Arc<dyn AgentTool>>,
}

impl ToolGroup {
    /// The web's `ToolkitSpec.matches`: the surface is one of the group's,
    /// and every required capability is on (absent means off).
    pub fn matches(&self, surface: Surface, caps: &Capabilities) -> bool {
        self.surfaces.contains(surface) && self.requires.iter().all(|c| caps.has(*c))
    }
}

/// What the model is told about a tool.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// The registered name (`get_quote`, `place_order`).
    pub name: &'static str,
    /// The description the model reads; the web's docstring, verbatim.
    pub description: Cow<'static, str>,
    /// The arguments as a JSON schema object (`type`, `properties`,
    /// `required`), the shape agno builds from the web's signature.
    pub parameters: Value,
    /// The run pauses for the trader's approval before this tool runs (the
    /// web's `requires_confirmation_tools`). Static: content can never waive
    /// it.
    pub needs_approval: bool,
}

impl ToolSpec {
    /// The arguments the tool cannot run without (`required` in the schema;
    /// the web reads them off the method signature).
    pub fn required(&self) -> Vec<&str> {
        self.parameters
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }
}

/// How a tool call ended, as the model reads it.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    /// The tool ran. The text is what the model reads: a JSON result from
    /// [`super::json::dumps_capped`] or a one-line confirmation.
    Ok(String),
    /// The call was refused or failed in a way the model can act on (the
    /// web's `RetryAgentRun`): the text names what to correct. The run goes
    /// on; the stream reports the call as not ok.
    Retry(String),
}

impl ToolOutcome {
    /// A result serialised safely and within [`MAX_RESULT_CHARS`].
    pub fn json(value: &Value) -> ToolOutcome {
        ToolOutcome::Ok(super::json::dumps_capped(value, MAX_RESULT_CHARS))
    }

    /// An argument the tool itself refuses, worded as every refusal is (the
    /// web's `invalid_argument`).
    pub fn invalid_argument(field: &str, reason: &str, fix: Option<&str>) -> ToolOutcome {
        let mut message = format!("The '{}' argument is invalid: {}", field, reason);
        if let Some(fix) = fix.filter(|f| !f.is_empty()) {
            message.push(' ');
            message.push_str(fix);
        }
        ToolOutcome::Retry(message)
    }

    pub fn text(&self) -> &str {
        match self {
            ToolOutcome::Ok(t) | ToolOutcome::Retry(t) => t,
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, ToolOutcome::Ok(_))
    }
}

/// One Agent tool.
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn spec(&self) -> &ToolSpec;

    /// Run the tool. Arguments have already been normalised and checked
    /// against the schema (`agent::tool_guard`): a call missing a required
    /// argument never reaches here. Long work watches `ctx.cancel`.
    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome;
}

/// Everything a tool knows about the run it serves (the web's
/// `ToolContext` plus what `tools/base.py` gives a toolkit).
pub struct ToolCtx {
    /// The app: services, databases, the in-process `/api/v1`.
    pub app: Arc<AppState>,
    pub surface: Surface,
    /// The `ag_conversation` row, for audit rows; `None` on a surface that
    /// stores nothing.
    pub conversation_id: Option<i64>,
    pub run_id: String,
    pub session_id: String,
    /// The signed-in OpenAlgo user.
    pub user_id: String,
    /// The trader's OpenAlgo API key: the in-process `/api/v1` resolves the
    /// user, broker and session from it. Never in an argument, a prompt or
    /// a result.
    pub api_key: Secret,
    pub caps: Capabilities,
    /// Analyzer mode when the run was built (the risk guard and the prompt;
    /// it never selects tools).
    pub analyzer_mode: bool,
    /// The trader's own message for this turn: the web search taint
    /// boundary builds every outbound query from it.
    pub operator_message: String,
    /// What the `/trading` panel sent about its chart (chart surface).
    pub chart_context: Option<Map<String, Value>>,
    /// Where a rendering tool leaves its frame (charts, cards, OpenUI,
    /// chart commands), so the payload never travels through the model.
    pub viz: VizSink,
    /// Cancelled when the trader stops the turn, the client goes away, the
    /// turn runs past its limit, or the app locks.
    pub cancel: CancellationToken,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("surface", &self.surface)
            .field("conversation_id", &self.conversation_id)
            .field("run_id", &self.run_id)
            .field("caps", &self.caps)
            .field("analyzer_mode", &self.analyzer_mode)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surfaces_parse_like_the_web() {
        assert_eq!(Surface::parse(" Chart "), Some(Surface::Chart));
        assert_eq!(Surface::parse("VOICE"), Some(Surface::Voice));
        assert_eq!(Surface::parse("panel"), None);
        assert!(Surfaces::CHAT_AND_VOICE.contains(Surface::Voice));
        assert!(!Surfaces::CHAT_AND_VOICE.contains(Surface::Chart));
    }

    #[test]
    fn a_group_needs_its_surface_and_every_capability() {
        fn none(_: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
            Vec::new()
        }
        let orders = ToolGroup {
            key: "orders",
            surfaces: Surfaces::ALL,
            requires: &[Capability::Trading],
            order: 50,
            description: "",
            build: none,
        };
        let off = Capabilities::default();
        let on = Capabilities {
            trading_enabled: true,
            ..Default::default()
        };
        assert!(!orders.matches(Surface::Chat, &off));
        assert!(orders.matches(Surface::Chart, &on));
        let chart = ToolGroup {
            key: "chart",
            surfaces: Surfaces::CHART_ONLY,
            requires: &[],
            order: 5,
            description: "",
            build: none,
        };
        assert!(!chart.matches(Surface::Chat, &on));
    }

    #[test]
    fn a_refused_argument_reads_as_the_web_words_it() {
        assert_eq!(
            ToolOutcome::invalid_argument("exchange", "NSEX is not an exchange.", Some("Use NSE.")),
            ToolOutcome::Retry(
                "The 'exchange' argument is invalid: NSEX is not an exchange. Use NSE.".into()
            )
        );
        assert_eq!(
            ToolOutcome::invalid_argument("x", "bad.", None).text(),
            "The 'x' argument is invalid: bad."
        );
    }
}
