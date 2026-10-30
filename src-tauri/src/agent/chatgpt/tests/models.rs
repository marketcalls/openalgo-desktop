//! The plan-model supplement (the web's `test_agent_chatgpt_models.py`).
//!
//! LiteLLM 1.106.6 ships every model the supplement used to add, so the
//! real list is empty and these guards would run on nothing. As on the web
//! (8a2fbf1d0), the tests put the last entry back beside whatever the list
//! holds, so the guards keep running against the shape the next one will
//! have.

use crate::agent::catalog::{self, load, Entry};
use crate::agent::chatgpt::models::{entry, register, register_from, PROVIDER, SUPPLEMENTAL};
use std::collections::HashMap;

/// The provider listing the catalogue keeps.
type Listing = Vec<(String, Vec<String>)>;

/// The last model the supplement carried before LiteLLM shipped it.
const STAND_IN: (&str, u64) = ("gpt-6-astra", 922_000);

/// The stand-in beside the real list (the web's `_a_supplement` fixture).
fn supplement() -> Vec<(&'static str, u64)> {
    let mut list = vec![STAND_IN];
    list.extend(SUPPLEMENTAL.iter().copied());
    list
}

fn add(e: &mut HashMap<String, Entry>, p: &mut Listing) -> Vec<String> {
    register_from(&supplement(), e, p)
}

fn empty() -> (HashMap<String, Entry>, Listing) {
    (
        HashMap::new(),
        vec![(PROVIDER.to_string(), vec!["chatgpt/gpt-5.4".to_string()])],
    )
}

#[test]
fn every_supplemental_model_is_in_the_catalogue_a_run_resolves_through() {
    let c = catalog::current();
    for (name, max_input) in SUPPLEMENTAL {
        let id = format!("chatgpt/{}", name);
        let e = c.entry(&id).unwrap_or_else(|| panic!("{} missing", id));
        assert_eq!(e.provider, PROVIDER);
        assert_eq!(e.mode.as_deref(), Some("responses"), "load-bearing");
        assert_eq!(e.max_input_tokens, Some(*max_input));
        assert_eq!(
            (e.input_cost, e.output_cost, e.cache_read_cost),
            (None, None, None)
        );
        assert!(c.list_models(PROVIDER, true).iter().any(|m| m.id == id));
        assert_eq!(
            c.estimate_cost(&id, 1000, 10, 0),
            None,
            "a plan turn has no price"
        );
    }
    assert!(c.entry("chatgpt/gpt-5.4").is_some(), "LiteLLM's own stay");
}

/// Web `test_the_subscription_models_are_offered_by_the_catalogue` and
/// `test_agent_chatgpt_wiring.py::...supports_function_calling`, at
/// 1.106.6: the bundled catalogue lists the plan's newest models itself,
/// none reads as unable to call tools (the Agent is tool driven), only
/// `gpt-rosalind-discovery` carries no answer ("tool support unknown"),
/// and none has a price.
#[test]
fn the_plan_models_come_from_the_bundled_catalogue_with_their_tool_support() {
    let c = load::bundled();
    let models = c.list_models(PROVIDER, true);
    assert!(models.len() >= 10, "{} plan models", models.len());
    for name in [
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-sol",
        "gpt-rosalind-discovery",
    ] {
        let id = format!("chatgpt/{}", name);
        let e = c.entry(&id).unwrap_or_else(|| panic!("{} not bundled", id));
        assert_eq!(e.provider, PROVIDER, "{}", id);
        assert_eq!(e.mode.as_deref(), Some("responses"), "{}", id);
    }
    let tools = |m: &catalog::ModelInfo| m.entry.as_ref().and_then(|e| e.supports_function_calling);
    let unable: Vec<&str> = models
        .iter()
        .filter(|m| tools(m) == Some(false))
        .map(|m| m.id.as_str())
        .collect();
    assert!(unable.is_empty(), "{:?}", unable);
    let unknown: Vec<&str> = models
        .iter()
        .filter(|m| tools(m).is_none())
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(unknown, ["chatgpt/gpt-rosalind-discovery"]);
    assert!(models
        .iter()
        .all(|m| m.entry.as_ref().is_none_or(|e| e.input_cost.is_none())));
}

/// The real list registers exactly what it holds (nothing at 1.106.6).
#[test]
fn the_real_supplement_registers_what_it_lists() {
    let mut e = HashMap::new();
    let mut p: Listing = Vec::new();
    assert_eq!(register(&mut e, &mut p).len(), SUPPLEMENTAL.len());
    assert_eq!(e.len(), SUPPLEMENTAL.len());
}

#[test]
fn registering_twice_adds_nothing_the_second_time() {
    let (mut e, mut p) = empty();
    assert_eq!(add(&mut e, &mut p).len(), supplement().len());
    assert!(add(&mut e, &mut p).is_empty());
    let listed = &p[0].1;
    assert_eq!(
        listed[0], "chatgpt/gpt-5.4",
        "the provider keeps what it listed"
    );
    assert_eq!(listed.len(), 1 + supplement().len());
}

#[test]
fn an_openai_entry_of_the_same_name_neither_blocks_nor_changes() {
    let (mut e, mut p) = empty();
    let (name, _) = STAND_IN;
    let openai = Entry {
        provider: "openai".into(),
        input_cost: Some(1e-6),
        ..Default::default()
    };
    e.insert(name.to_string(), openai.clone());
    let added = add(&mut e, &mut p);
    assert!(added.contains(&format!("chatgpt/{}", name)));
    assert_eq!(e[&format!("chatgpt/{}", name)].provider, PROVIDER);
    assert_eq!(e[name], openai);
}

#[test]
fn a_plan_entry_already_there_is_never_overwritten() {
    let (mut e, mut p) = empty();
    let (name, _) = STAND_IN;
    let own = Entry {
        provider: PROVIDER.into(),
        max_input_tokens: Some(1),
        ..Default::default()
    };
    e.insert(format!("chatgpt/{}", name), own.clone());
    assert!(!add(&mut e, &mut p).contains(&format!("chatgpt/{}", name)));
    assert_eq!(e[&format!("chatgpt/{}", name)], own);
    // Under its bare name too.
    let (mut e, mut p) = empty();
    e.insert(name.to_string(), own);
    assert!(!add(&mut e, &mut p).contains(&format!("chatgpt/{}", name)));
}

#[test]
fn no_entry_carries_a_price_and_every_one_answers_on_responses() {
    let e = entry(1000);
    assert_eq!(
        (e.input_cost, e.output_cost, e.cache_read_cost),
        (None, None, None)
    );
    assert_eq!(e.mode.as_deref(), Some("responses"));
    assert_eq!(e.supports_function_calling, Some(true));
}

#[test]
fn a_catalogue_of_another_shape_still_gets_the_models() {
    let mut e = HashMap::new();
    let mut p: Listing = Vec::new();
    assert_eq!(add(&mut e, &mut p).len(), supplement().len());
    assert_eq!(p[0].0, PROVIDER);
}

#[test]
fn the_refused_models_are_not_offered() {
    for refused in [
        "gpt-5.6",
        "gpt-5.6-cyber",
        "gpt-5.5-pro",
        "gpt-5.5-codex",
        "gpt-5.6-codex",
        "gpt-6",
        "gpt-6-pro",
        "gpt-6-astra-codex",
    ] {
        assert!(
            SUPPLEMENTAL.iter().all(|(n, _)| *n != refused),
            "{}",
            refused
        );
    }
}
