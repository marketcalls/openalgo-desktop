//! The Agent's tool registry (the web's `services/agent/tools/__init__.py`).
//!
//! [`TOOLKITS`] has one line per toolkit, each a [`ToolGroup`] naming its
//! surfaces, the capabilities it needs and its order. [`build_tools`] is
//! evaluated for every run against that run's surface and capabilities, so
//! a run without trading never sees an order tool in its schema at all, on
//! any surface. Selection never reads anything the model can influence.
//!
//! Adding a toolkit is one file under this folder plus one line below.
//! Each toolkit's tools reach OpenAlgo through [`service`] and take their
//! specs from [`specs`] (the web's wording, captured); every registered
//! toolkit is pinned against the web by `spec_contract`.
//!
//! Flow (`flow_gen`) and Python strategy generation (`strategy_gen`) are not
//! in this version (maintainer, 2026-10-27). Flow comes later: its line goes
//! in the marked place below, chat surface only, as on the web.

pub mod account;
pub mod answer;
pub mod instrument;
pub mod live;
pub mod market;
pub mod numbers;
pub mod options;
pub mod options_common;
pub mod service;
#[cfg(test)]
pub(crate) mod spec_contract;
pub mod specs;
pub mod symbols;
#[cfg(test)]
pub(crate) mod testkit;
pub mod values;

use crate::agent::contract::tool::{AgentTool, ToolCtx, ToolGroup};
use std::collections::HashSet;
use std::sync::Arc;

/// Every toolkit, one line each (`order` decides the model's order).
pub const TOOLKITS: &[ToolGroup] = &[
    market::GROUP,
    symbols::GROUP,
    account::GROUP,
    options::GROUP,
    instrument::GROUP,
    live::GROUP,
    // Toolkit lanes add their line here, for example:
    // orders::GROUP,     // order 50, every surface, requires Trading
    //
    // Later: flow_gen::GROUP (order 70, chat only), not in this version.
];

/// The groups a run is offered, ordered by `order` then `key`.
pub fn select<'a>(groups: &'a [ToolGroup], ctx: &ToolCtx) -> Vec<&'a ToolGroup> {
    let mut selected: Vec<&ToolGroup> = groups
        .iter()
        .filter(|g| g.matches(ctx.surface, &ctx.caps))
        .collect();
    selected.sort_by(|a, b| a.order.cmp(&b.order).then(a.key.cmp(b.key)));
    selected
}

/// Build the tools one run may use from `groups` (the app passes
/// [`TOOLKITS`]). A name already taken by an earlier tool is a programming
/// error: it is logged and the later tool skipped, so one name never means
/// two tools.
pub fn build_tools(groups: &[ToolGroup], ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let mut names: HashSet<&'static str> = HashSet::new();
    let mut tools: Vec<Arc<dyn AgentTool>> = Vec::new();
    for group in select(groups, ctx) {
        for tool in (group.build)(ctx) {
            let name = tool.spec().name;
            if names.insert(name) {
                tools.push(tool);
            } else {
                tracing::error!(
                    "Agent tool {} of toolkit {} repeats a name already offered; skipped",
                    name,
                    group.key
                );
            }
        }
    }
    tracing::debug!(
        "Agent tools for surface={} trading={} web_search={} mcp={}: {}",
        ctx.surface.as_str(),
        ctx.caps.trading_enabled,
        ctx.caps.web_search_enabled,
        ctx.caps.mcp_enabled,
        tools.len()
    );
    tools
}

#[cfg(test)]
pub(crate) mod testing {
    //! A context and stand-in tools for registry and run tests.
    use crate::agent::contract::tool::{
        AgentTool, Capabilities, Surface, ToolCtx, ToolOutcome, ToolSpec,
    };
    use crate::agent::contract::viz::VizSink;
    use crate::security::Secret;
    use crate::state::AppState;
    use async_trait::async_trait;
    use serde_json::{json, Map, Value};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    pub fn ctx(app: Arc<AppState>, surface: Surface, caps: Capabilities) -> ToolCtx {
        ToolCtx {
            app,
            surface,
            conversation_id: Some(1),
            run_id: "run-1".into(),
            session_id: "session-1".into(),
            user_id: "trader".into(),
            api_key: Secret::new("test-key"),
            caps,
            analyzer_mode: false,
            operator_message: String::new(),
            chart_context: None,
            viz: VizSink::new(),
            cancel: CancellationToken::new(),
        }
    }

    /// A tool that answers with its own name and the arguments it got.
    pub struct Echo(pub ToolSpec);

    impl Echo {
        pub fn named(name: &'static str, approval: bool) -> Arc<dyn AgentTool> {
            Arc::new(Echo(ToolSpec {
                name,
                description: format!("Echo {}.", name).into(),
                parameters: json!({"type": "object", "properties": {}, "required": []}),
                needs_approval: approval,
            }))
        }
    }

    #[async_trait]
    impl AgentTool for Echo {
        fn spec(&self) -> &ToolSpec {
            &self.0
        }
        async fn call(&self, _ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
            ToolOutcome::json(&json!({"tool": self.0.name, "args": args}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{ctx, Echo};
    use super::*;
    use crate::agent::contract::tool::{Capabilities, Capability, Surface, Surfaces};

    fn market(_: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
        vec![Echo::named("get_quote", false)]
    }
    fn orders(_: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
        vec![Echo::named("place_order", true)]
    }
    fn chart(_: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
        vec![
            Echo::named("draw_level", false),
            Echo::named("get_quote", false),
        ]
    }

    const GROUPS: &[ToolGroup] = &[
        ToolGroup {
            key: "orders",
            surfaces: Surfaces::ALL,
            requires: &[Capability::Trading],
            order: 50,
            description: "",
            build: orders,
        },
        ToolGroup {
            key: "market",
            surfaces: Surfaces::ALL,
            requires: &[],
            order: 10,
            description: "",
            build: market,
        },
        ToolGroup {
            key: "chart",
            surfaces: Surfaces::CHART_ONLY,
            requires: &[],
            order: 5,
            description: "",
            build: chart,
        },
    ];

    fn names(tools: &[Arc<dyn AgentTool>]) -> Vec<&'static str> {
        tools.iter().map(|t| t.spec().name).collect()
    }

    #[tokio::test]
    async fn a_run_sees_only_its_surface_and_capabilities_in_order() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        let off = Capabilities::default();
        let on = Capabilities {
            trading_enabled: true,
            ..Default::default()
        };
        let chat = build_tools(GROUPS, &ctx(t.ctx.clone(), Surface::Chat, off));
        assert_eq!(names(&chat), ["get_quote"]);
        let chat_trading = build_tools(GROUPS, &ctx(t.ctx.clone(), Surface::Chat, on));
        assert_eq!(names(&chat_trading), ["get_quote", "place_order"]);
        // The chart group comes first and its repeated name is skipped.
        let panel = build_tools(GROUPS, &ctx(t.ctx.clone(), Surface::Chart, on));
        assert_eq!(names(&panel), ["draw_level", "get_quote", "place_order"]);
    }

    #[test]
    fn the_registry_has_unique_keys() {
        let mut keys = HashSet::new();
        for g in TOOLKITS {
            assert!(keys.insert(g.key), "toolkit {} listed twice", g.key);
        }
    }
}
