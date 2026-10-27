use super::base_url::{host_class, validate_base_url};
use super::target::{target_for, Target};
use super::*;
use crate::agent::catalog;
use genai::adapter::AdapterKind;
use serde_json::Value;

#[test]
fn the_five_kinds_and_what_each_needs() {
    assert_eq!(
        PROVIDER_KINDS,
        &[
            "openai",
            "anthropic",
            "ollama",
            "openai_compatible",
            "litellm"
        ]
    );
    assert_eq!(
        provider_spec(" x ").unwrap_err(),
        "Unknown provider kind 'x'. Expected one of: openai, anthropic, ollama, openai_compatible, litellm"
    );
    let ollama = provider_spec("ollama").unwrap();
    assert!(!ollama.needs_key && ollama.needs_base_url);
    assert_eq!(provider_spec(" litellm ").unwrap().prefix, "");
}

#[test]
fn the_catalogue_id_puts_the_prefix_on_once() {
    assert_eq!(catalog_id("openai", " gpt-4o ").unwrap(), "openai/gpt-4o");
    assert_eq!(
        catalog_id("openai", "openai/gpt-4o").unwrap(),
        "openai/gpt-4o"
    );
    assert_eq!(
        catalog_id("openai_compatible", "Qwen").unwrap(),
        "openai/Qwen"
    );
    assert_eq!(catalog_id("litellm", "groq/llama").unwrap(), "groq/llama");
    assert_eq!(
        catalog_id("ollama", "llama3.1:8b").unwrap(),
        "ollama/llama3.1:8b"
    );
    assert_eq!(
        catalog_id("openai", "  ").unwrap_err(),
        "A model name is required"
    );
}

#[test]
fn the_gate_names_the_first_problem() {
    let v = |k: &str, n: &str, u: Option<&str>, key: bool| validate_provider_config(k, n, u, key);
    assert!(v("bogus", "", None, false)
        .unwrap()
        .starts_with("Unknown provider kind 'bogus'"));
    assert_eq!(
        v("openai", " ", None, false).unwrap(),
        "A model name is required"
    );
    assert_eq!(
        v("ollama", "m", None, false).unwrap(),
        "Ollama requires a base URL"
    );
    assert_eq!(
        v("openai_compatible", "m", Some("  /"), true).unwrap(),
        "OpenAI-compatible endpoint requires a base URL"
    );
    assert_eq!(
        v("ollama", "m", Some("ftp://x"), false).unwrap(),
        "The base URL must start with http:// or https://"
    );
    assert_eq!(
        v("openai", "gpt-4o", None, false).unwrap(),
        "OpenAI requires an API key"
    );
    assert_eq!(
        v("litellm", "groq/x", None, false).unwrap(),
        "Groq requires an API key"
    );
    assert_eq!(
        v("litellm", "x", None, false).unwrap(),
        "This provider requires an API key"
    );
    assert_eq!(v("openai", "gpt-4o", None, true), None);
    assert_eq!(v("ollama", "m", Some("HTTP://h:1"), false), None);
}

/// The web's TestTheProviderAsksForNoKey: the plan's prefix, not the kind,
/// is what needs no key.
#[test]
fn only_a_plan_model_needs_no_key_among_the_keyed_kinds() {
    assert_eq!(
        validate_provider_config("litellm", "chatgpt/gpt-5.4", None, false),
        None
    );
    for (kind, name) in [
        ("litellm", "openai/gpt-5.4"),
        ("litellm", "gpt-5.4"),
        ("openai", "gpt-5.4"),
    ] {
        let e = validate_provider_config(kind, name, None, false).unwrap();
        assert!(e.contains("API key"), "{} {}", kind, name);
    }
}

/// D-027: a local OpenAI-compatible server (LM Studio, vLLM, llamafile)
/// works without a key, as its catalogue card says; the web refused it.
#[test]
fn an_openai_compatible_endpoint_needs_no_key() {
    assert_eq!(
        validate_provider_config(
            "openai_compatible",
            "qwen",
            Some("http://127.0.0.1:1234/v1"),
            false
        ),
        None
    );
    let c = catalog::current();
    for card in c.list_providers() {
        if !card.needs_key {
            let spec = provider_spec(&card.provider_kind).unwrap();
            let plan = card.id == "chatgpt";
            assert!(
                !spec.needs_key || plan,
                "{} says keyless but its kind asks for a key",
                card.id
            );
        }
    }
}

#[test]
fn the_endpoint_guard_is_the_one_both_dialogs_run() {
    let cases: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/agent/base_url_cases.json"
    ))
    .unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let url = case["url"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        let got = validate_base_url(Some(url), kind);
        match case["error"].as_str() {
            Some(error) => assert_eq!(got.unwrap_err(), error, "{:?}", url),
            None => {
                let stored = got.unwrap();
                assert_eq!(stored.as_deref(), case["normalized"].as_str(), "{:?}", url);
                if let (Some(stored), Some(class)) = (stored, case["class"].as_str()) {
                    let parsed = url::Url::parse(&stored).unwrap();
                    assert_eq!(host_class(&parsed).unwrap().as_str(), class, "{:?}", url);
                }
            }
        }
    }
    assert_eq!(validate_base_url(None, "openai"), Ok(None));
}

/// The web's per-kind rule: both kinds that take an endpoint may reach a
/// server on this computer or the local network, however its address is
/// spelled; the metadata service is refused for every kind.
#[test]
fn both_endpoint_kinds_reach_local_servers_and_never_the_metadata_service() {
    for kind in ["ollama", "openai_compatible"] {
        for url in [
            "http://127.0.0.1:11434",
            "http://localhost:1234/v1",
            "http://2130706433:1234",
            "http://0x7f.1:1234",
            "http://[::ffff:127.0.0.1]:1234",
            "http://192.168.1.10:1234",
        ] {
            assert!(
                validate_base_url(Some(url), kind).is_ok(),
                "{} {}",
                kind,
                url
            );
        }
    }
    for kind in PROVIDER_KINDS {
        assert_eq!(
            validate_base_url(Some("http://0251.0376.0251.0376/"), kind).unwrap_err(),
            super::base_url::METADATA
        );
    }
}

fn t(kind: &str, name: &str, url: Option<&str>) -> Result<Target, String> {
    target_for(&catalog::current(), kind, name, url)
}

#[test]
fn each_kind_reaches_its_provider_through_genai() {
    let openai = t("openai", "gpt-4o-mini", None).unwrap();
    assert_eq!(
        (
            openai.adapter,
            openai.endpoint.as_str(),
            openai.model.as_str()
        ),
        (
            AdapterKind::OpenAI,
            "https://api.openai.com/v1/",
            "gpt-4o-mini"
        )
    );
    let claude = t("anthropic", "anthropic/claude-sonnet-4-20250514", None).unwrap();
    assert_eq!(claude.adapter, AdapterKind::Anthropic);
    assert_eq!(claude.model, "claude-sonnet-4-20250514");
    let local = t("ollama", "llama3.1:8b", Some("http://127.0.0.1:11434/")).unwrap();
    assert_eq!(
        (local.adapter, local.endpoint.as_str(), local.model.as_str()),
        (
            AdapterKind::Ollama,
            "http://127.0.0.1:11434/",
            "llama3.1:8b"
        )
    );
    let lm = t(
        "openai_compatible",
        "qwen2.5",
        Some("http://localhost:1234/v1"),
    )
    .unwrap();
    assert_eq!(
        (lm.adapter, lm.endpoint.as_str()),
        (AdapterKind::OpenAI, "http://localhost:1234/v1/")
    );
    let groq = t("litellm", "groq/llama-3.3-70b-versatile", None).unwrap();
    assert_eq!(
        (groq.adapter, groq.endpoint.as_str(), groq.model.as_str()),
        (
            AdapterKind::Groq,
            "https://api.groq.com/openai/v1/",
            "llama-3.3-70b-versatile"
        )
    );
    let router = t("litellm", "openrouter/anthropic/claude-3.5-sonnet", None).unwrap();
    assert_eq!(router.model, "anthropic/claude-3.5-sonnet");
    let plan = t("litellm", "chatgpt/gpt-5.4", None).unwrap();
    assert_eq!(plan.adapter, AdapterKind::OpenAIResp);
    let bare = t("litellm", "gpt-4o", None).unwrap();
    assert_eq!(
        bare.adapter,
        AdapterKind::OpenAI,
        "a bare name is placed by the catalogue"
    );
}

#[test]
fn a_provider_the_desktop_cannot_reach_is_refused_by_name() {
    let e = t("litellm", "bedrock/anthropic.claude-3", None).unwrap_err();
    assert!(e.contains("'bedrock'") && !e.contains("LiteLLM"), "{}", e);
    let e = t("litellm", "nobody-knows-this-model", None).unwrap_err();
    assert!(e.contains("groq/llama-3.3-70b-versatile"), "{}", e);
}

/// The web's TestAModelThatCannotSeeIsRefusedByName and the reasoning
/// twin: the catalogue decides for a model it knows, the box for one it has
/// never heard of.
#[test]
fn the_catalogue_overrules_a_wrong_box_and_the_box_decides_the_unknown() {
    let c = catalog::current();
    assert!(capable(&c, "gpt-4o", false, Probe::Vision));
    assert!(!capable(&c, "gpt-3.5-turbo", true, Probe::Vision));
    assert!(!capable(&c, "openai/gpt-3.5-turbo", true, Probe::Vision));
    assert!(capable(&c, "openai/gpt-4o", false, Probe::Vision));
    let unknown = "ollama/some-local-build-that-does-not-exist";
    assert!(capable(&c, unknown, true, Probe::Vision));
    assert!(!capable(&c, unknown, false, Probe::Vision));
    assert!(!capable(&c, "gpt-3.5-turbo", true, Probe::Reasoning));
}
