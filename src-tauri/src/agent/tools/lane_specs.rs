//! What the model is told about the skills, OpenScript and external data
//! tools, exactly as the web tells it.
//!
//! `lane_specs.json` holds those six tools' entries copied from the tools
//! part's `specs.json` (generated from the web by
//! `scripts/agent/capture_web_tools.py`), with the same reader, so the
//! wording is never retyped. When this branch sits on the tools part, the
//! file and this module go and the toolkits call `specs::web_or_log`.

use crate::agent::contract::tool::ToolSpec;
use serde_json::Value;
use std::borrow::Cow;
use std::sync::LazyLock;

static SPECS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("lane_specs.json")).unwrap_or_else(|e| {
        tracing::error!("The Agent's tool specs could not be read: {}", e);
        Value::Null
    })
});

fn web(name: &str) -> Option<ToolSpec> {
    let (key, entry) = SPECS.get("tools")?.as_object()?.get_key_value(name)?;
    Some(ToolSpec {
        name: key.as_str(),
        description: Cow::Borrowed(entry.get("description")?.as_str()?),
        parameters: entry.get("parameters")?.clone(),
        needs_approval: entry.get("needs_approval")?.as_bool()?,
    })
}

/// The web's spec for tool `name` of `toolkit`, or `None` (logged), so the
/// toolkit offers its other tools.
pub fn web_or_log(toolkit: &str, name: &str) -> Option<ToolSpec> {
    let spec = web(name);
    if spec.is_none() {
        tracing::error!(
            "Agent toolkit {} has no web spec for {}; the tool is not offered",
            toolkit,
            name
        );
    }
    spec
}

#[cfg(test)]
mod tests {
    //! The three toolkits and their six tools pinned against the web's
    //! capture in `tests/fixtures/agent/tools.json` (the tools part's
    //! `spec_contract` does the same for every toolkit once the branches
    //! meet).
    use crate::agent::contract::tool::{Capabilities, Capability, Surface};
    use crate::agent::tools::testing::ctx;
    use crate::agent::tools::{openscript_gen, skills};
    use serde_json::Value;

    fn fixture() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/agent/tools.json"
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_toolkits_and_their_tools_are_the_webs() {
        let f = fixture();
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        let caps = Capabilities {
            mcp_enabled: true,
            ..Default::default()
        };
        for group in [&skills::GROUP, &openscript_gen::GROUP] {
            let web = f["toolkits"]
                .as_array()
                .unwrap()
                .iter()
                .find(|k| k["key"] == group.key)
                .unwrap();
            for s in [Surface::Chat, Surface::Chart, Surface::Voice] {
                let listed = web["surfaces"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == s.as_str());
                assert_eq!(
                    group.surfaces.contains(s),
                    listed,
                    "{} on {}",
                    group.key,
                    s.as_str()
                );
            }
            let requires: Vec<&str> = group
                .requires
                .iter()
                .map(|c| match c {
                    Capability::Mcp => "mcp_enabled",
                    Capability::Trading => "trading_enabled",
                    Capability::WebSearch => "web_search_enabled",
                })
                .collect();
            assert_eq!(
                serde_json::to_value(requires).unwrap(),
                web["requires"],
                "{}",
                group.key
            );
            assert_eq!(Some(u64::from(group.order)), web["order"].as_u64());
            assert_eq!(Some(group.description), web["description"].as_str());
            let tools = (group.build)(&ctx(t.ctx.clone(), Surface::Chat, caps));
            let names: Vec<&str> = tools.iter().map(|x| x.spec().name).collect();
            assert_eq!(
                serde_json::to_value(&names).unwrap(),
                web["tools"],
                "{}",
                group.key
            );
            for tool in &tools {
                let spec = tool.spec();
                let w = f["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|x| x["name"] == spec.name)
                    .unwrap();
                assert_eq!(
                    spec.description.as_ref(),
                    w["description"].as_str().unwrap(),
                    "{}",
                    spec.name
                );
                assert_eq!(spec.parameters, w["parameters"], "{}", spec.name);
                assert_eq!(
                    Some(spec.needs_approval),
                    w["needs_approval"].as_bool(),
                    "{}",
                    spec.name
                );
            }
        }
        assert_eq!(super::SPECS["web_commit"], f["web_commit"]);
    }
}
