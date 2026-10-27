//! What the model is told about each tool, exactly as the web tells it.
//!
//! `specs.json` beside this file is generated from the web by
//! `scripts/agent/capture_web_tools.py`: agno builds every web toolkit as a
//! web run does, and the file keeps, per tool of the toolkits this version
//! builds, the description and the JSON-schema arguments the model is sent
//! (agno's shape and order) and whether the run pauses for the trader's
//! approval. A tool takes its [`ToolSpec`] from here with [`web`], so the
//! wording is never retyped, and `spec_contract` pins every registered tool
//! against the web capture in `tests/fixtures/agent/tools.json`.

use crate::agent::contract::tool::ToolSpec;
use serde_json::Value;
use std::borrow::Cow;
use std::sync::LazyLock;

static SPECS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("specs.json")).unwrap_or_else(|e| {
        tracing::error!("The Agent's tool specs could not be read: {}", e);
        Value::Null
    })
});

/// The web's spec for tool `name`, or `None` when the web has no such tool
/// (a programming error the contract test catches; the toolkit logs it and
/// offers the tool's siblings).
pub fn web(name: &str) -> Option<ToolSpec> {
    let (key, entry) = SPECS.get("tools")?.as_object()?.get_key_value(name)?;
    Some(ToolSpec {
        name: key.as_str(),
        description: Cow::Borrowed(entry.get("description")?.as_str()?),
        parameters: entry.get("parameters")?.clone(),
        needs_approval: entry.get("needs_approval")?.as_bool()?,
    })
}

/// [`web`], logging the miss: the form a toolkit's `build` uses.
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
    use super::*;
    use crate::agent::tools::spec_contract;

    /// The shipped specs are the capture's, tool for tool, for every toolkit
    /// of this version (Python strategy generation and Flow left out).
    #[test]
    fn the_shipped_specs_are_the_web_capture() {
        let fixture = spec_contract::fixture();
        let mut expected = Vec::new();
        for t in fixture["tools"].as_array().into_iter().flatten() {
            let toolkit = t["toolkit"].as_str().unwrap_or_default();
            if toolkit == "strategy_gen" || toolkit == "flow_gen" {
                assert!(web(t["name"].as_str().unwrap_or_default()).is_none());
                continue;
            }
            let name = t["name"].as_str().unwrap_or_default();
            let spec = web(name).unwrap_or_else(|| panic!("no shipped spec for {}", name));
            spec_contract::assert_tool(&spec);
            expected.push(name);
        }
        assert_eq!(expected.len(), 59);
        assert_eq!(SPECS["tools"].as_object().map(|m| m.len()), Some(59));
        assert_eq!(SPECS["web_commit"], fixture["web_commit"]);
    }

    #[test]
    fn an_unknown_tool_has_no_spec() {
        assert!(web("get_quote_of_the_day").is_none());
        assert!(web_or_log("market", "nope").is_none());
        let q = web("get_quote").unwrap_or_else(|| panic!("get_quote"));
        assert_eq!(q.name, "get_quote");
        assert!(!q.needs_approval);
        assert_eq!(q.required(), vec!["symbol", "exchange"]);
        assert!(web("place_order").is_some_and(|s| s.needs_approval));
    }
}
