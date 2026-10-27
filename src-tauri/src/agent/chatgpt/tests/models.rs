//! The plan-model supplement (the web's `test_agent_chatgpt_models.py`).

use crate::agent::catalog::{self, Entry};
use crate::agent::chatgpt::models::{entry, register, PROVIDER, SUPPLEMENTAL};
use std::collections::HashMap;

/// The provider listing the catalogue keeps.
type Listing = Vec<(String, Vec<String>)>;

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

#[test]
fn registering_twice_adds_nothing_the_second_time() {
    let (mut e, mut p) = empty();
    assert_eq!(register(&mut e, &mut p).len(), SUPPLEMENTAL.len());
    assert!(register(&mut e, &mut p).is_empty());
    let listed = &p[0].1;
    assert_eq!(
        listed[0], "chatgpt/gpt-5.4",
        "the provider keeps what it listed"
    );
    assert_eq!(listed.len(), 1 + SUPPLEMENTAL.len());
}

#[test]
fn an_openai_entry_of_the_same_name_neither_blocks_nor_changes() {
    let (mut e, mut p) = empty();
    let (name, _) = SUPPLEMENTAL[0];
    let openai = Entry {
        provider: "openai".into(),
        input_cost: Some(1e-6),
        ..Default::default()
    };
    e.insert(name.to_string(), openai.clone());
    assert_eq!(register(&mut e, &mut p).len(), SUPPLEMENTAL.len());
    assert_eq!(e[name], openai);
}

#[test]
fn a_plan_entry_already_there_is_never_overwritten() {
    let (mut e, mut p) = empty();
    let (name, _) = SUPPLEMENTAL[0];
    let own = Entry {
        provider: PROVIDER.into(),
        max_input_tokens: Some(1),
        ..Default::default()
    };
    e.insert(format!("chatgpt/{}", name), own.clone());
    assert!(register(&mut e, &mut p).is_empty());
    assert_eq!(e[&format!("chatgpt/{}", name)], own);
    // Under its bare name too.
    let (mut e, mut p) = empty();
    e.insert(name.to_string(), own);
    assert!(register(&mut e, &mut p).is_empty());
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
    assert_eq!(register(&mut e, &mut p).len(), SUPPLEMENTAL.len());
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
