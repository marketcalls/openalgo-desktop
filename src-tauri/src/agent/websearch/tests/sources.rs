//! The fetch provider and the preferred sources in the settings (the web's
//! `test_agent_websearch_settings.py`, the cases web 46af9591f added), and
//! the defaults against the fixture recorded from the web.

use super::ctx;
use crate::agent::websearch::settings::{self, prepare, Config, WriteError};
use crate::agent::websearch::sources::{from_row, normalise};
use crate::agent::websearch::{
    fetch_spec, provider_spec, FETCH_PROVIDERS, KEY_DAILY_CAP, KEY_FETCH_PROVIDER,
    KEY_MAX_CALLS_PER_TURN, KEY_PERPLEXITY_MODEL, KEY_PREFERRED_SOURCES, KEY_PROVIDER, KEY_USAGE,
};
use serde_json::{json, Map, Value};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/agent/settings.json");

fn body(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

const DEFAULT: [&str; 3] = ["screener.in", "scanx.trade", "tickertape.in"];

/// Web `test_the_literals_match_the_tool_module_they_were_copied_from`
/// and `test_the_preferred_sources_key_matches_the_tool_module`, against
/// the fixture recorded from the web's settings module.
#[test]
fn the_keys_and_defaults_are_the_webs() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(
        fixture["websearch_keys"],
        json!([
            KEY_PROVIDER,
            KEY_FETCH_PROVIDER,
            KEY_PREFERRED_SOURCES,
            KEY_PERPLEXITY_MODEL,
            KEY_MAX_CALLS_PER_TURN,
            KEY_DAILY_CAP,
            KEY_USAGE
        ])
    );
    assert_eq!(fixture["websearch_defaults"], settings::defaults());
}

/// Web `test_firecrawl_and_tinyfish_are_keyed_link_providers_that_also_fetch`.
#[test]
fn firecrawl_and_tinyfish_are_keyed_link_providers_that_also_fetch() {
    for provider in ["firecrawl", "tinyfish"] {
        let spec = provider_spec(provider).unwrap();
        assert!(spec.needs_key && spec.fetch && spec.result_kind == "links");
        assert_eq!(spec.tool, "web_search");
    }
    assert!(!provider_spec("tavily").unwrap().fetch);
    assert_eq!(FETCH_PROVIDERS, ["firecrawl", "tinyfish"]);
}

/// Web `test_config_reports_the_fetch_provider_and_whether_fetch_can_run`
/// and `test_fetch_is_not_ready_when_only_a_non_fetching_provider_has_a_key`.
#[tokio::test]
async fn the_config_reports_the_fetch_provider_and_whether_fetch_can_run() {
    let t = ctx();
    settings::set_key(&t.ctx, "tavily", Some(&json!("tvly-test-0123456789"))).unwrap();
    let c = settings::config(&t.ctx).unwrap();
    assert_eq!(c["fetch_provider"], "firecrawl");
    assert_eq!(c["fetch_ready"], json!(false));
    settings::set_key(&t.ctx, "tinyfish", Some(&json!("tf-test-0123456789"))).unwrap();
    let c = settings::config(&t.ctx).unwrap();
    assert_eq!(c["fetch_ready"], json!(true));
    assert_eq!(settings::defaults()["fetch_provider"], "firecrawl");
}

/// Web `test_a_provider_that_cannot_read_a_page_is_refused_as_the_fetch_provider`.
#[test]
fn a_provider_that_cannot_read_a_page_is_refused_as_the_fetch_provider() {
    for provider in ["tavily", "perplexity", "duckduckgo", "bing", ""] {
        assert_eq!(
            prepare(&body(json!({ "fetch_provider": provider }))).unwrap_err(),
            "fetch_provider must be one of: firecrawl, tinyfish",
            "{}",
            provider
        );
    }
    assert_eq!(fetch_spec(" TinyFish ").unwrap().id, "tinyfish");
    assert_eq!(
        prepare(&body(json!({"fetch_provider": null}))).unwrap_err(),
        "fetch_provider must be one of: firecrawl, tinyfish"
    );
}

/// Web `test_a_fetch_provider_choice_is_validated_before_anything_is_written`
/// and `test_preferred_sources_are_validated_before_anything_is_written`.
#[tokio::test]
async fn a_bad_choice_writes_nothing() {
    let t = ctx();
    let bad = [
        json!({"daily_cap": 50, "fetch_provider": "tavily"}),
        json!({"daily_cap": 10, "preferred_sources": ["screener.in", "nonsense"]}),
        json!({"daily_cap": 10, "preferred_sources": "a.com b.com c.com d.com e.com f.com"}),
        json!({"daily_cap": 10, "preferred_sources": 5}),
    ];
    for v in bad {
        assert!(matches!(
            settings::update(&t.ctx, &body(v.clone())),
            Err(WriteError::Invalid(_))
        ));
        let c = Config::read(&t.ctx.sqlite.conn().unwrap());
        assert_eq!(c.daily_cap, 200, "{} wrote something", v);
    }
}

/// Web `test_a_pasted_address_is_reduced_to_its_domain`.
#[test]
fn a_pasted_address_is_reduced_to_its_domain() {
    for (typed, expected) in [
        ("screener.in", "screener.in"),
        ("https://www.Screener.in/company/TCS/", "screener.in"),
        ("  SCANX.trade  ", "scanx.trade"),
        ("http://user@scanx.trade:8080/x?y=1", "scanx.trade"),
    ] {
        assert_eq!(normalise(&json!(typed)).unwrap(), expected);
    }
}

/// Web `test_something_that_is_not_a_website_is_refused`.
#[test]
fn something_that_is_not_a_website_is_refused() {
    let long = format!("{}.in", "x".repeat(200));
    for typed in [
        "",
        "screener",
        "not a site",
        "a..b",
        "-x.com",
        long.as_str(),
    ] {
        let e = normalise(&json!(typed)).unwrap_err();
        assert!(e.contains("not a website address"), "{}: {}", typed, e);
    }
    assert_eq!(
        normalise(&json!("not a site")).unwrap_err(),
        "'not a site' is not a website address such as screener.in"
    );
}

/// Web `test_preferred_sources_default_to_three_in_priority_order_and_empty_turns_them_off`.
#[test]
fn preferred_sources_default_to_three_in_order_and_empty_turns_them_off() {
    assert_eq!(from_row(None), DEFAULT);
    assert_eq!(from_row(Some("")), DEFAULT);
    assert!(from_row(Some("[]")).is_empty());
    assert_eq!(from_row(Some(r#"["scanx.trade"]"#)), ["scanx.trade"]);
}

/// Web `test_a_damaged_preference_row_reads_as_the_default`.
#[test]
fn a_damaged_preference_row_reads_as_the_default() {
    for raw in ["not json", r#"{"a": 1}"#, r#"["not a site"]"#] {
        assert_eq!(from_row(Some(raw)), DEFAULT, "{}", raw);
    }
}

/// Web `test_the_config_reports_the_preferred_sources`, and a stored list
/// read back as the tools read it.
#[tokio::test]
async fn the_config_reports_the_preferred_sources() {
    let t = ctx();
    let c = settings::config(&t.ctx).unwrap();
    assert_eq!(c["preferred_sources"], json!(DEFAULT));
    assert_eq!(settings::defaults()["preferred_sources"], json!(DEFAULT));
    settings::update(
        &t.ctx,
        &body(json!({"preferred_sources": "https://www.Tickertape.in/x, screener.in screener.in"})),
    )
    .unwrap();
    let c = Config::read(&t.ctx.sqlite.conn().unwrap());
    assert_eq!(c.preferred_sources, ["tickertape.in", "screener.in"]);
    let stored: String = t
        .ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT value FROM ag_setting WHERE key = ?1",
            [KEY_PREFERRED_SOURCES],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stored, r#"["tickertape.in", "screener.in"]"#,
        "the web's json.dumps"
    );
    settings::update(&t.ctx, &body(json!({"preferred_sources": []}))).unwrap();
    assert!(Config::read(&t.ctx.sqlite.conn().unwrap())
        .preferred_sources
        .is_empty());
}

/// A stored fetch provider is read leniently, as the search provider is.
#[test]
fn a_stored_fetch_provider_is_read_leniently() {
    let rows = |v: &str| -> std::collections::HashMap<String, String> {
        [(KEY_FETCH_PROVIDER.to_string(), v.to_string())].into()
    };
    assert_eq!(
        Config::from_rows(&rows("TinyFish")).fetch_provider,
        "tinyfish"
    );
    assert_eq!(
        Config::from_rows(&rows("tavily")).fetch_provider,
        "firecrawl"
    );
    assert_eq!(Config::from_rows(&rows("")).fetch_provider, "firecrawl");
}
