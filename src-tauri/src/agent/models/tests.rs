//! The model registry as the web's `agent_db` and model routes keep it.

use super::secrets::{self, fingerprint};
use super::service;
use super::store::{self, ModelChanges, NewModel};
use crate::agent::reply::Refusal;
use crate::security::Unlocked;
use serde_json::{json, Map, Value};

fn ctx() -> crate::state::testing::TestCtx {
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    t.ctx.security.install(&Unlocked::random().unwrap());
    t
}

fn body(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn new(kind: &str, name: &str, url: Option<&str>) -> NewModel {
    NewModel {
        provider_kind: kind.into(),
        model_name: name.into(),
        display_name: name.into(),
        base_url: url.map(str::to_string),
        default_reasoning_effort: "off".into(),
        ..Default::default()
    }
}

#[test]
fn a_fingerprint_reads_as_the_webs() {
    assert_eq!(fingerprint("short"), "...????");
    assert_eq!(fingerprint("12345678"), "...????");
    // sha256("sk-test-1234567890abcd") = 4f7d..., first 12 hex digits.
    let f = fingerprint("sk-test-1234567890abcd");
    assert!(f.starts_with("...abcd sha256:"), "{}", f);
    assert_eq!(f.len(), "...abcd sha256:".len() + 12);
}

#[tokio::test]
async fn a_secret_is_sealed_to_its_name_and_never_rewritten_unchanged() {
    let t = ctx();
    let conn = t.ctx.sqlite.conn().unwrap();
    let sec = &t.ctx.security;
    assert_eq!(
        secrets::set(&conn, sec, "", "x"),
        Err("A secret name is required")
    );
    assert_eq!(
        secrets::set(&conn, sec, "provider:openai", ""),
        Err("A secret value is required")
    );
    secrets::set(&conn, sec, "provider:openai", "sk-first-key-0001").unwrap();
    let stored: String = conn
        .query_row("SELECT ciphertext FROM ag_secret", [], |r| r.get(0))
        .unwrap();
    assert!(stored.starts_with("v1:") && !stored.contains("sk-first"));
    secrets::mark_used(&conn, "provider:openai", t.ctx.now());
    let before: i64 = conn
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    secrets::set(&conn, sec, "provider:openai", "sk-first-key-0001").unwrap();
    let after: i64 = conn
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "an unchanged key is not rewritten");
    secrets::set(&conn, sec, "provider:openai", "sk-second-key-002").unwrap();
    let listed = secrets::list(&conn).unwrap();
    assert_eq!(
        listed[0].last_used_at, None,
        "a replaced key has not been used"
    );
    assert_eq!(listed[0].fingerprint, fingerprint("sk-second-key-002"));
    assert_eq!(
        secrets::get(&conn, sec, "provider:openai")
            .unwrap()
            .expose(),
        "sk-second-key-002"
    );
    // Moved to another name it does not open.
    conn.execute(
        "INSERT INTO ag_secret (name, ciphertext) SELECT 'model:9', ciphertext FROM ag_secret",
        [],
    )
    .unwrap();
    assert!(secrets::get(&conn, sec, "model:9").is_none());
    // Locked keys open nothing.
    sec.lock();
    assert!(secrets::get(&conn, sec, "provider:openai").is_none());
}

#[tokio::test]
async fn the_registry_keeps_one_tested_enabled_default() {
    let t = ctx();
    let conn = t.ctx.sqlite.conn().unwrap();
    let now = t.ctx.now();
    let a = store::create(&conn, &new("openai", "gpt-4o", None), now).unwrap();
    assert!(!a.is_default, "a new model is never the default");
    assert_eq!(
        store::create(&conn, &new("openai", "gpt-4o", None), now).unwrap_err(),
        "gpt-4o is already registered for this provider",
        "no base URL is a duplicate too (SQLite's NULLs would miss it)"
    );
    assert_eq!(
        store::set_default(&conn, a.id).unwrap_err(),
        "Test the model's credentials before making it the default"
    );
    store::record_test(&conn, a.id, true, None, now).unwrap();
    assert!(
        store::get(&conn, a.id).unwrap().unwrap().is_default,
        "the first pass promotes"
    );
    assert!(store::is_configured(&conn).unwrap());
    let b = store::create(&conn, &new("anthropic", "claude", None), now).unwrap();
    store::record_test(&conn, b.id, true, None, now).unwrap();
    assert!(
        !store::get(&conn, b.id).unwrap().unwrap().is_default,
        "one default"
    );
    store::set_default(&conn, b.id).unwrap();
    assert!(!store::get(&conn, a.id).unwrap().unwrap().is_default);
    // A failing default hands it on.
    store::record_test(&conn, b.id, false, Some("bad key"), now).unwrap();
    let b_row = store::get(&conn, b.id).unwrap().unwrap();
    assert_eq!(b_row.last_test_error.as_deref(), Some("bad key"));
    assert!(!b_row.is_default && store::get(&conn, a.id).unwrap().unwrap().is_default);
    // Disabling the default hands it on (to nothing tested: unconfigured).
    let off = ModelChanges {
        enabled: Some(false),
        ..Default::default()
    };
    store::update(&conn, a.id, &off, now).unwrap();
    assert!(!store::is_configured(&conn).unwrap());
    assert_eq!(
        store::set_default(&conn, a.id).unwrap_err(),
        "Enable the model before making it the default"
    );
    assert_eq!(
        store::record_test(&conn, 999, true, None, now).unwrap_err(),
        "Model not found"
    );
    let failed = store::record_test(&conn, b.id, false, None, now);
    assert!(failed.is_ok());
    assert_eq!(
        store::get(&conn, b.id)
            .unwrap()
            .unwrap()
            .last_test_error
            .as_deref(),
        Some("The provider rejected the request")
    );
}

#[tokio::test]
async fn an_edit_keeps_identity_and_a_new_endpoint_is_tested_again() {
    let t = ctx();
    let conn = t.ctx.sqlite.conn().unwrap();
    let now = t.ctx.now();
    let m = store::create(
        &conn,
        &new("ollama", "llama3", Some("http://127.0.0.1:11434")),
        now,
    )
    .unwrap();
    store::record_test(&conn, m.id, true, None, now).unwrap();
    let refuse = |c: ModelChanges| store::update(&conn, m.id, &c, now).unwrap_err();
    assert_eq!(
        refuse(ModelChanges {
            is_default: true,
            ..Default::default()
        }),
        "Use the default action to change which model is the default"
    );
    assert_eq!(
        refuse(ModelChanges {
            model_name: Some(json!("other")),
            ..Default::default()
        }),
        "A model's provider and model name cannot be changed. Register the other model instead."
    );
    let same = ModelChanges {
        provider_kind: Some(json!("ollama")),
        display_name: Some(" Llama ".into()),
        ..Default::default()
    };
    assert_eq!(
        store::update(&conn, m.id, &same, now).unwrap().display_name,
        "Llama"
    );
    assert_eq!(
        refuse(ModelChanges {
            base_url: Some(None),
            ..Default::default()
        }),
        "A base URL is required for ollama"
    );
    let moved = store::update(
        &conn,
        m.id,
        &ModelChanges {
            base_url: Some(Some("http://10.0.0.2:11434".into())),
            ..Default::default()
        },
        now,
    )
    .unwrap();
    assert_eq!(moved.last_test_ok, None);
    assert!(!moved.is_default);
    let twin = store::create(
        &conn,
        &new("ollama", "llama3", Some("http://127.0.0.1:11434")),
        now,
    )
    .unwrap();
    assert_eq!(
        store::update(
            &conn,
            twin.id,
            &ModelChanges {
                base_url: Some(Some("http://10.0.0.2:11434".into())),
                ..Default::default()
            },
            now
        )
        .unwrap_err(),
        "llama3 is already registered at that endpoint"
    );
}

#[tokio::test]
async fn deleting_a_model_takes_its_own_key_and_hands_the_default_on() {
    let t = ctx();
    let conn = t.ctx.sqlite.conn().unwrap();
    let now = t.ctx.now();
    let a = store::create(&conn, &new("openai", "gpt-4o", None), now).unwrap();
    let b = store::create(&conn, &new("openai", "gpt-4.1", None), now).unwrap();
    store::record_test(&conn, a.id, true, None, now).unwrap();
    store::record_test(&conn, b.id, true, None, now).unwrap();
    secrets::set(
        &conn,
        &t.ctx.security,
        &format!("model:{}", a.id),
        "sk-own-key-123456",
    )
    .unwrap();
    secrets::set(
        &conn,
        &t.ctx.security,
        "provider:openai",
        "sk-shared-key-1234",
    )
    .unwrap();
    store::delete(&conn, a.id).unwrap();
    assert!(secrets::get(&conn, &t.ctx.security, &format!("model:{}", a.id)).is_none());
    assert!(secrets::get(&conn, &t.ctx.security, "provider:openai").is_some());
    assert!(store::get(&conn, b.id).unwrap().unwrap().is_default);
    assert_eq!(store::delete(&conn, a.id).unwrap_err(), "Model not found");
}

#[tokio::test]
async fn registering_reads_the_request_as_the_web_does() {
    let t = ctx();
    let refused = |b: Value| -> Refusal { service::create(&t.ctx, &body(b)).unwrap_err() };
    assert_eq!(
        refused(json!({"provider_kind": "azure", "model_name": "x"})).message(),
        "provider_kind must be one of: openai, anthropic, ollama, openai_compatible, litellm"
    );
    assert_eq!(
        refused(json!({"provider_kind": "openai"})).message(),
        "A model name is required"
    );
    assert_eq!(
        refused(json!({"provider_kind": "ollama", "model_name": "m"})).message(),
        "Ollama requires a base URL"
    );
    assert_eq!(
        refused(
            json!({"provider_kind": "openai", "model_name": "m", "default_reasoning_effort": "max"})
        )
        .message(),
        "default_reasoning_effort must be one of: off, low, medium, high"
    );
    assert_eq!(
        refused(json!({"provider_kind": "openai", "model_name": "m", "api_key_scope": "team"}))
            .message(),
        "api_key_scope must be one of: provider, model"
    );
    let r = refused(json!({"provider_kind": "openai", "model_name": "m".repeat(201)}));
    assert_eq!(
        (r.status, r.message()),
        (400, "A model name can be at most 200 characters")
    );
    let made = service::create(
        &t.ctx,
        &body(json!({"provider_kind": "openai", "model_name": " gpt-4o ", "api_key": "sk-shared-key-1234",
                     "supports_vision": 1, "enabled": "", "is_default": true})),
    )
    .unwrap();
    let m = &made["data"];
    assert_eq!(made["status"], "success");
    assert_eq!(
        (m["model_name"].as_str(), m["display_name"].as_str()),
        (Some("gpt-4o"), Some("gpt-4o"))
    );
    assert_eq!(
        (m["is_default"].clone(), m["enabled"].clone()),
        (json!(false), json!(false))
    );
    assert_eq!(m["supports_vision"], json!(true));
    assert_eq!(m["api_key_source"], "provider:openai");
    assert_eq!(
        m["api_key_fingerprint"],
        json!(fingerprint("sk-shared-key-1234"))
    );
    assert!(m.get("api_key").is_none() && !made.to_string().contains("sk-shared"));
    let dup = refused(json!({"provider_kind": "openai", "model_name": "gpt-4o"}));
    assert_eq!(
        (dup.status, dup.message()),
        (409, "gpt-4o is already registered for this provider")
    );
    // A second model of the provider needs no key: the shared one answers.
    let second = service::create(
        &t.ctx,
        &body(json!({"provider_kind": "openai", "model_name": "gpt-4.1"})),
    )
    .unwrap();
    assert_eq!(second["data"]["has_api_key"], json!(true));
    let own = service::create(
        &t.ctx,
        &body(json!({"provider_kind": "openai", "model_name": "o3", "api_key": "sk-own-key-123456", "api_key_scope": "MODEL"})),
    )
    .unwrap();
    assert_eq!(
        own["data"]["api_key_source"],
        json!(format!("model:{}", own["data"]["id"]))
    );
}

#[tokio::test]
async fn the_model_json_is_the_webs_shape_with_reasoning_resolved() {
    let t = ctx();
    let made = service::create(
        &t.ctx,
        &body(json!({"provider_kind": "openai", "model_name": "gpt-3.5-turbo", "supports_reasoning": true})),
    )
    .unwrap();
    let keys: Vec<&String> = made["data"].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "id",
            "provider_kind",
            "model_name",
            "display_name",
            "base_url",
            "enabled",
            "is_default",
            "supports_reasoning",
            "default_reasoning_effort",
            "supports_vision",
            "tools_unreliable",
            "last_tested_at",
            "last_test_ok",
            "last_test_error",
            "has_api_key",
            "api_key_fingerprint",
            "api_key_source",
            "created_at",
            "updated_at"
        ]
    );
    assert_eq!(
        made["data"]["supports_reasoning"],
        json!(false),
        "the catalogue knows it cannot"
    );
    assert!(made["data"]["created_at"]
        .as_str()
        .unwrap()
        .ends_with("+00:00"));
    let listed = service::list(&t.ctx).unwrap();
    assert_eq!(listed["data"][0]["supports_reasoning"], json!(false));
}

#[tokio::test]
async fn an_edit_keeps_a_blank_key_and_refuses_an_empty_request() {
    let t = ctx();
    let made = service::create(
        &t.ctx,
        &body(json!({"provider_kind": "openai", "model_name": "gpt-4o", "api_key": "sk-shared-key-1234"})),
    )
    .unwrap();
    let id = made["data"]["id"].as_i64().unwrap();
    let nothing = service::update(&t.ctx, id, &body(json!({"api_key": "  "}))).unwrap_err();
    assert_eq!(
        (nothing.status, nothing.message()),
        (400, "Nothing to update")
    );
    let kept = service::update(
        &t.ctx,
        id,
        &body(json!({"display_name": "GPT", "api_key": ""})),
    )
    .unwrap();
    assert_eq!(
        kept["data"]["api_key_fingerprint"],
        json!(fingerprint("sk-shared-key-1234"))
    );
    let missing = service::update(&t.ctx, 999, &body(json!({"display_name": "x"}))).unwrap_err();
    assert_eq!(
        (missing.status, missing.message()),
        (404, "Model not found")
    );
    let bad_url = service::update(
        &t.ctx,
        id,
        &body(json!({"base_url": "http://169.254.169.254"})),
    )
    .unwrap_err();
    assert_eq!(bad_url.status, 400);
    let replaced = service::update(
        &t.ctx,
        id,
        &body(json!({"api_key": "sk-model-key-98765", "api_key_scope": "model"})),
    )
    .unwrap();
    assert_eq!(
        replaced["data"]["api_key_source"],
        json!(format!("model:{}", id))
    );
    let gone = service::delete(&t.ctx, id).unwrap();
    assert_eq!(
        gone,
        json!({"status": "success", "message": "Model deleted"})
    );
    assert_eq!(service::delete(&t.ctx, id).unwrap_err().status, 404);
    let untested = service::create(
        &t.ctx,
        &body(json!({"provider_kind": "openai", "model_name": "x"})),
    )
    .unwrap();
    let r = service::set_default(&t.ctx, untested["data"]["id"].as_i64().unwrap()).unwrap_err();
    assert_eq!(
        (r.status, r.message()),
        (
            409,
            "Test the model's credentials before making it the default"
        )
    );
    assert_eq!(service::set_default(&t.ctx, 999).unwrap_err().status, 404);
}

#[tokio::test]
async fn a_fresh_start_takes_every_agent_key_and_the_tests_that_rested_on_them() {
    let t = ctx();
    let conn = t.ctx.sqlite.conn().unwrap();
    let now = t.ctx.now();
    let keyed = store::create(&conn, &new("openai", "gpt-4o", None), now).unwrap();
    let local = store::create(
        &conn,
        &new("ollama", "llama3", Some("http://127.0.0.1:11434")),
        now,
    )
    .unwrap();
    secrets::set(
        &conn,
        &t.ctx.security,
        "provider:openai",
        "sk-shared-key-1234",
    )
    .unwrap();
    store::record_test(&conn, keyed.id, true, None, now).unwrap();
    store::record_test(&conn, local.id, true, None, now).unwrap();
    secrets::wipe(&conn).unwrap();
    assert!(secrets::list(&conn).unwrap().is_empty());
    assert_eq!(
        store::get(&conn, keyed.id).unwrap().unwrap().last_test_ok,
        None
    );
    assert_eq!(
        store::get(&conn, local.id).unwrap().unwrap().last_test_ok,
        Some(true)
    );
}

/// A test the gate refuses is a failed test on the row, at 0 ms, and no
/// request; a provider that does not answer in time fails the test with
/// the limit in the sentence.
#[tokio::test]
async fn a_test_refused_before_sending_or_unanswered_is_recorded_failed() {
    use crate::agent::testing::{FakeProvider, Reply};
    let t = ctx();
    let keyless = service::create(
        &t.ctx,
        &body(json!({"provider_kind": "openai", "model_name": "gpt-4o"})),
    )
    .unwrap();
    let id = keyless["data"]["id"].as_i64().unwrap();
    let v = super::probe::test(&t.ctx, id).await.unwrap();
    assert_eq!(
        (
            v["ok"].clone(),
            v["message"].clone(),
            v["latency_ms"].clone()
        ),
        (json!(false), json!("OpenAI requires an API key"), json!(0))
    );
    assert_eq!(
        v["data"]["last_test_error"],
        json!("OpenAI requires an API key")
    );
    let unreachable = service::create(
        &t.ctx,
        &body(json!({"provider_kind": "litellm", "model_name": "bedrock/x", "api_key": "sk-shared-key-1234"})),
    )
    .unwrap();
    let v = super::probe::test(&t.ctx, unreachable["data"]["id"].as_i64().unwrap())
        .await
        .unwrap();
    assert_eq!(v["ok"], json!(false));
    assert!(
        v.get("data").is_none(),
        "the web's kwargs path carries no row"
    );
    let fake = FakeProvider::start(vec![Reply::Hang]).await;
    let slow = service::create(
        &t.ctx,
        &body(
            json!({"provider_kind": "ollama", "model_name": "llama3", "base_url": fake.endpoint()}),
        ),
    )
    .unwrap();
    let v = super::probe::test_within(
        &t.ctx,
        slow["data"]["id"].as_i64().unwrap(),
        std::time::Duration::from_millis(300),
    )
    .await
    .unwrap();
    assert_eq!(v["ok"], json!(false));
    assert_eq!(
        v["message"],
        json!("The provider did not answer within 1 second")
    );
    assert_eq!(
        super::probe::test(&t.ctx, 999).await.unwrap_err().status,
        404
    );
}
