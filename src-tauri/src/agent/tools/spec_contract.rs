//! Every toolkit's specs pinned against the web's (test only).
//!
//! `tests/fixtures/agent/tools.json` is recorded from the web by
//! `scripts/agent/capture_web_tools.py`: agno builds each web toolkit as a
//! web run does, and the file keeps what the model is sent for every tool
//! (name, description, JSON-schema arguments in agno's order), the surfaces
//! it is offered on, its toolkit and whether it needs the trader's approval.
//!
//! [`assert_toolkit`] checks one [`ToolGroup`] and the tools it builds
//! against that file, byte for byte. Every group in [`TOOLKITS`] is checked
//! by the test below, so a toolkit lane that adds its line to the registry
//! is pinned without writing anything else; a lane may also call it from its
//! own tests while the line is not in yet.

use crate::agent::contract::tool::{
    AgentTool, Capabilities, Capability, Surface, ToolCtx, ToolGroup, ToolSpec,
};
use crate::agent::tools::TOOLKITS;
use serde_json::Value;
use std::sync::{Arc, LazyLock};

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/agent/tools.json"
));

static WEB: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(FIXTURE).expect("tools.json is JSON"));

/// The whole recorded fixture.
pub fn fixture() -> &'static Value {
    &WEB
}

/// The web's entry for toolkit `key` (`market`, `orders`).
pub fn toolkit(key: &str) -> &'static Value {
    fixture()["toolkits"]
        .as_array()
        .and_then(|all| all.iter().find(|t| t["key"] == key))
        .unwrap_or_else(|| panic!("the web has no toolkit {:?}", key))
}

/// The web's entry for tool `name`.
pub fn tool(name: &str) -> &'static Value {
    fixture()["tools"]
        .as_array()
        .and_then(|all| all.iter().find(|t| t["name"] == name))
        .unwrap_or_else(|| panic!("the web has no tool {:?}", name))
}

fn capability_name(c: Capability) -> &'static str {
    match c {
        Capability::Trading => "trading_enabled",
        Capability::WebSearch => "web_search_enabled",
        Capability::Mcp => "mcp_enabled",
    }
}

const SURFACES: [Surface; 3] = [Surface::Chat, Surface::Chart, Surface::Voice];

/// One tool's spec equals the web's: name, description, the arguments as
/// the model reads them (property order included) and the approval flag.
pub fn assert_tool(spec: &ToolSpec) {
    let web = tool(spec.name);
    assert_eq!(
        spec.description.as_ref(),
        web["description"].as_str().unwrap_or_default(),
        "description of {}",
        spec.name
    );
    let ours = serde_json::to_string(&spec.parameters).unwrap_or_default();
    let theirs = serde_json::to_string(&web["parameters"]).unwrap_or_default();
    assert_eq!(ours, theirs, "arguments of {}", spec.name);
    assert_eq!(
        Some(spec.needs_approval),
        web["needs_approval"].as_bool(),
        "approval of {}",
        spec.name
    );
}

/// A toolkit and the tools it builds equal the web's: key, surfaces,
/// required capabilities, order, description, the tools in the web's order,
/// and each tool's spec ([`assert_tool`]).
pub fn assert_toolkit(group: &ToolGroup, ctx: &ToolCtx) {
    let web = toolkit(group.key);
    for s in SURFACES {
        let listed = web["surfaces"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == s.as_str()));
        assert_eq!(
            group.surfaces.contains(s),
            listed,
            "{} on surface {}",
            group.key,
            s.as_str()
        );
    }
    let mut requires: Vec<&str> = group.requires.iter().map(|c| capability_name(*c)).collect();
    requires.sort_unstable();
    let web_requires: Vec<&str> = web["requires"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(requires, web_requires, "capabilities of {}", group.key);
    assert_eq!(
        Some(u64::from(group.order)),
        web["order"].as_u64(),
        "order of {}",
        group.key
    );
    assert_eq!(
        Some(group.description),
        web["description"].as_str(),
        "description of toolkit {}",
        group.key
    );
    let tools: Vec<Arc<dyn AgentTool>> = (group.build)(ctx);
    let names: Vec<&str> = tools.iter().map(|t| t.spec().name).collect();
    let web_names: Vec<&str> = web["tools"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(names, web_names, "tools of {}", group.key);
    for t in &tools {
        assert_tool(t.spec());
    }
}

/// Every capability on, so every group's surface alone decides.
pub fn all_capabilities() -> Capabilities {
    Capabilities {
        trading_enabled: true,
        web_search_enabled: true,
        mcp_enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::{build_tools, testing};

    #[test]
    fn the_fixture_holds_every_web_tool() {
        let f = fixture();
        assert_eq!(f["toolkits"].as_array().map(Vec::len), Some(18));
        assert_eq!(f["tools"].as_array().map(Vec::len), Some(63));
        assert_eq!(f["surfaces"]["chat"].as_array().map(Vec::len), Some(53));
        assert_eq!(f["surfaces"]["chart"].as_array().map(Vec::len), Some(58));
        assert_eq!(f["surfaces"]["voice"].as_array().map(Vec::len), Some(45));
        assert_eq!(f["approval"].as_array().map(Vec::len), Some(10));
    }

    /// Each registered toolkit equals the web's, and on every surface the
    /// registered tools come in the web's order.
    #[tokio::test]
    async fn every_registered_toolkit_matches_the_web() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        for group in TOOLKITS {
            let surface = SURFACES
                .into_iter()
                .find(|s| group.surfaces.contains(*s))
                .unwrap_or(Surface::Chat);
            let ctx = testing::ctx(t.ctx.clone(), surface, all_capabilities());
            assert_toolkit(group, &ctx);
        }
        for surface in SURFACES {
            let ctx = testing::ctx(t.ctx.clone(), surface, all_capabilities());
            let ours: Vec<&str> = build_tools(TOOLKITS, &ctx)
                .iter()
                .map(|t| t.spec().name)
                .collect();
            let web: Vec<&str> = fixture()["surfaces"][surface.as_str()]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .filter(|n| ours.contains(n))
                .collect();
            assert_eq!(ours, web, "tools on surface {}", surface.as_str());
        }
    }

    #[test]
    fn a_drifted_spec_is_caught() {
        let web = tool("get_funds");
        let mut spec = ToolSpec {
            name: "get_funds",
            description: web["description"]
                .as_str()
                .unwrap_or_default()
                .to_string()
                .into(),
            parameters: web["parameters"].clone(),
            needs_approval: false,
        };
        assert_tool(&spec);
        spec.description = "Fetch the funds.".into();
        assert!(std::panic::catch_unwind(|| assert_tool(&spec)).is_err());
    }
}
