//! The prompt section that teaches the model what each external data
//! server is for (the web's `services/agent/mcp/prompt.py`): generated from
//! the registry, and added only to a run offered the external data tools,
//! so a surface is never taught a tool it does not have.

use super::registry::McpServerSpec;
use crate::agent::contract::policy::SectionSource;
use crate::agent::contract::tool::{Capabilities, Surface};
use crate::agent::prompts::PromptSection;
use crate::state::AppState;
use std::collections::BTreeSet;
use std::sync::Arc;

pub const MCP_SECTION_KEY: &str = "mcp";

/// Straight after USING TOOLS (20) and before the domain sections: at the
/// end of a long prompt the model answered exchange statistics from web
/// search instead.
pub const MCP_SECTION_ORDER: i64 = 22;

const RULES: &str = "- The broker tools come first for live quotes, intraday candles, depth, option
  chains, funds, positions and orders. Use these servers for what the broker
  does not give.
- Each server below is the publisher's own data, not a website about it. When
  the operator names that publisher or asks for a figure it publishes (market
  breadth, gainers and losers, a 52-week high or low, a corporate action, an
  index valuation), call the server. Never answer those from web search or
  web research, which restate the same numbers second hand; keep web search for
  news, commentary and events.
- Pass the server key exactly. When unsure of a tool's name or arguments, call
  list_mcp_tools first, with tool set for one tool's full arguments. Cached.
- If a server fails, say the source is unavailable right now and answer from
  the broker tools where you can. Do not retry it more than once.";

fn server_line(spec: &McpServerSpec) -> String {
    let mut line = format!("- {} ({}): use for {}.", spec.key, spec.title, spec.use_for);
    if !spec.caveats.is_empty() {
        line.push(' ');
        line.push_str(&spec.caveats);
    }
    line
}

/// The section for the servers offered on a surface, or `None` when there
/// are none.
pub fn mcp_prompt_section(servers: &[Arc<McpServerSpec>]) -> Option<PromptSection> {
    if servers.is_empty() {
        return None;
    }
    let mut lines =
        vec!["Read-only data services, reached with list_mcp_tools and call_mcp_tool.".to_string()];
    lines.extend(servers.iter().map(|s| server_line(s)));
    lines.push(RULES.to_string());
    let providers: BTreeSet<&str> = servers
        .iter()
        .map(|s| s.provider.as_str())
        .filter(|p| !p.is_empty())
        .collect();
    if !providers.is_empty() {
        lines.push(format!(
            "- Name {} as the source. The data is published for information and education.",
            providers.into_iter().collect::<Vec<_>>().join(" and ")
        ));
    }
    Some(PromptSection::new(
        MCP_SECTION_KEY,
        "EXTERNAL DATA (MCP)",
        &lines.join("\n"),
        MCP_SECTION_ORDER,
    ))
}

/// The EXTERNAL DATA section, on a run whose external data switch is on
/// (the run's capability: the trader's setting and a server registered for
/// the surface).
pub struct ExternalDataSection;

impl SectionSource for ExternalDataSection {
    fn sections(
        &self,
        app: &AppState,
        surface: Surface,
        caps: &Capabilities,
    ) -> Vec<PromptSection> {
        if !caps.mcp_enabled {
            return Vec::new();
        }
        mcp_prompt_section(&app.agent.mcp().for_surface(surface))
            .into_iter()
            .collect()
    }
}
