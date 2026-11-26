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
//!
//! Both captured files stay what the capture wrote at their `web_commit`.
//! A web change to a description that the desktop ships before the next
//! capture is a hunk in `carried.json` (written by hand), applied here as
//! the specs load and by the contract to the capture it compares with, so
//! a re-capture never loses it and a capture's stamp stays true.

use crate::agent::contract::tool::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use std::sync::LazyLock;

/// One web change carried onto a captured description: `from` becomes
/// `to` in tool `tool`'s text, as web commit `web_commit` changed it.
#[derive(Deserialize)]
struct Hunk {
    tool: String,
    web_commit: String,
    from: String,
    to: String,
}

#[derive(Deserialize)]
struct Carried {
    hunks: Vec<Hunk>,
}

static CARRIED: LazyLock<Vec<Hunk>> = LazyLock::new(|| {
    serde_json::from_str::<Carried>(include_str!("carried.json"))
        .map(|c| c.hunks)
        .unwrap_or_else(|e| {
            tracing::error!("The Agent's carried tool wording could not be read: {}", e);
            Vec::new()
        })
});

static SPECS: LazyLock<Value> = LazyLock::new(|| {
    let mut specs = serde_json::from_str(include_str!("specs.json")).unwrap_or_else(|e| {
        tracing::error!("The Agent's tool specs could not be read: {}", e);
        Value::Null
    });
    if let Some(tools) = specs.get_mut("tools").and_then(Value::as_object_mut) {
        for (name, entry) in tools.iter_mut() {
            if let Some(Value::String(text)) = entry.get_mut("description") {
                *text = carry(name, text);
            }
        }
    }
    specs
});

/// `captured`, the capture's description of tool `name`, with the hunks
/// `carried.json` holds for it.
pub(super) fn carry(name: &str, captured: &str) -> String {
    apply(&CARRIED, name, captured)
}

/// `captured` with each of `hunks` for tool `name`. A hunk whose `from` is
/// not in the text exactly once is left out and logged: the capture changed
/// under it, and `a_carried_hunk_applies_once_to_the_untouched_capture`
/// fails.
fn apply(hunks: &[Hunk], name: &str, captured: &str) -> String {
    let mut text = captured.to_string();
    for hunk in hunks.iter().filter(|h| h.tool == name) {
        if text.matches(hunk.from.as_str()).count() == 1 {
            text = text.replacen(hunk.from.as_str(), &hunk.to, 1);
        } else {
            tracing::error!(
                "Agent tool {}: the wording carried from web {} no longer fits its capture",
                name,
                hunk.web_commit
            );
        }
    }
    text
}

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
    /// of this version (Python strategy generation left out; Flow's two
    /// tools in since 1.0.5).
    #[test]
    fn the_shipped_specs_are_the_web_capture() {
        let fixture = spec_contract::fixture();
        let mut expected = Vec::new();
        for t in fixture["tools"].as_array().into_iter().flatten() {
            let toolkit = t["toolkit"].as_str().unwrap_or_default();
            if toolkit == "strategy_gen" {
                assert!(web(t["name"].as_str().unwrap_or_default()).is_none());
                continue;
            }
            let name = t["name"].as_str().unwrap_or_default();
            let spec = web(name).unwrap_or_else(|| panic!("no shipped spec for {}", name));
            spec_contract::assert_tool(&spec);
            expected.push(name);
        }
        assert_eq!(expected.len(), 62);
        assert_eq!(SPECS["tools"].as_object().map(|m| m.len()), Some(62));
        assert!(web("validate_flow").is_some_and(|s| !s.needs_approval));
        assert!(web("save_flow").is_some_and(|s| s.needs_approval));
        assert_eq!(SPECS["web_commit"], fixture["web_commit"]);
    }

    /// The captured files keep the web's words at their stamp, and what the
    /// desktop carries is declared: every hunk's `from` is in the captured
    /// description exactly once and its `to` is not, in the fixture and in
    /// `specs.json` alike, it comes from a web commit other than the stamp,
    /// and the shipped spec says `to`. A capture from a web commit that
    /// already has a hunk fails here until its entry leaves `carried.json`.
    /// Review of the openalgo-charts 2.7.5 bump: its count hunk had been
    /// typed into both captured files, which then matched no web commit.
    #[test]
    fn a_carried_hunk_applies_once_to_the_untouched_capture() {
        let fixture = spec_contract::fixture();
        let generated: Value = serde_json::from_str(include_str!("specs.json")).unwrap_or_default();
        let mut carried = Vec::new();
        for hunk in CARRIED.iter() {
            let name = hunk.tool.as_str();
            let captured = [
                spec_contract::tool(name)["description"].as_str(),
                generated["tools"][name]["description"].as_str(),
            ];
            for text in captured.map(Option::unwrap_or_default) {
                assert_eq!(
                    text.matches(hunk.from.as_str()).count(),
                    1,
                    "{}: {:?}",
                    name,
                    hunk.from
                );
                assert!(!text.contains(hunk.to.as_str()), "{}: {:?}", name, hunk.to);
            }
            assert_ne!(fixture["web_commit"], hunk.web_commit.as_str());
            let shipped = web(name).unwrap_or_else(|| panic!("no shipped spec for {}", name));
            assert!(shipped.description.contains(hunk.to.as_str()), "{}", name);
            assert!(
                !shipped.description.contains(hunk.from.as_str()),
                "{}",
                name
            );
            carried.push((name, hunk.web_commit.as_str()));
        }
        carried.dedup();
        assert_eq!(carried, [("list_chart_indicators", "501198289")]);
    }

    /// A hunk applies to its own tool where its `from` stands once; one the
    /// capture no longer has room for (gone, or there twice) is left out,
    /// never forced, and the others still apply.
    #[test]
    fn a_hunk_that_no_longer_fits_leaves_the_capture_as_it_is() {
        let hunk = |from: &str, to: &str| Hunk {
            tool: "list_chart_indicators".into(),
            web_commit: "501198289".into(),
            from: from.into(),
            to: to.into(),
        };
        let hunks = [hunk("of 112", "of 113"), hunk("four of", "five of")];
        let name = "list_chart_indicators";
        let both = "a library of 112, and four of them";
        assert_eq!(
            apply(&hunks, name, both),
            "a library of 113, and five of them"
        );
        assert_eq!(apply(&hunks, "get_quote", both), both);
        let gone = "a library of 128, and four of them";
        assert_eq!(
            apply(&hunks, name, gone),
            "a library of 128, and five of them"
        );
        let twice = "of 112 here, of 112 there";
        assert_eq!(apply(&hunks, name, twice), twice);
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
