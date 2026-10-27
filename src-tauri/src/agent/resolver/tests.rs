//! Which model a run gets, and the setup gate, as the web's `builder.py`
//! and `agent_status` decide them.

use super::*;
use crate::agent::contract::model::NoCredentials;
use crate::agent::models::store::NewModel;
use crate::agent::plan::{PlanSignIn, NOT_SIGNED_IN};
use crate::security::Unlocked;
use genai::adapter::AdapterKind;
use serde_json::json;

fn ctx() -> crate::state::testing::TestCtx {
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    t.ctx.security.install(&Unlocked::random().unwrap());
    t
}

fn add(t: &crate::state::testing::TestCtx, kind: &str, name: &str, url: Option<&str>) -> ModelRow {
    let conn = t.ctx.sqlite.conn().unwrap();
    store::create(
        &conn,
        &NewModel {
            provider_kind: kind.into(),
            model_name: name.into(),
            display_name: name.to_uppercase(),
            base_url: url.map(str::to_string),
            default_reasoning_effort: "off".into(),
            ..Default::default()
        },
        t.ctx.now(),
    )
    .unwrap()
}

fn pass(t: &crate::state::testing::TestCtx, id: i64) {
    let conn = t.ctx.sqlite.conn().unwrap();
    store::record_test(&conn, id, true, None, t.ctx.now()).unwrap();
}

async fn resolve(
    t: &crate::state::testing::TestCtx,
    id: Option<i64>,
) -> Result<ResolvedModel, ModelError> {
    t.ctx.agent.resolver().resolve(&t.ctx, id).await
}

#[tokio::test]
async fn nothing_configured_nothing_named_and_nothing_switched_off_are_refused() {
    let t = ctx();
    assert!(!t.ctx.agent.resolver().is_configured(&t.ctx));
    assert_eq!(
        resolve(&t, None).await.unwrap_err(),
        ModelError::NotConfigured(NOT_CONFIGURED_MESSAGE.into())
    );
    assert_eq!(
        resolve(&t, Some(41)).await.unwrap_err(),
        ModelError::NotFound(
            "Model 41 is not registered. Choose one of the configured models.".into()
        )
    );
    let m = add(&t, "openai", "gpt-4o", None);
    let conn = t.ctx.sqlite.conn().unwrap();
    store::update(
        &conn,
        m.id,
        &store::ModelChanges {
            enabled: Some(false),
            ..Default::default()
        },
        t.ctx.now(),
    )
    .unwrap();
    assert_eq!(
        resolve(&t, Some(m.id)).await.unwrap_err(),
        ModelError::Disabled(
            "Model GPT-4O is disabled. Enable it in agent settings or choose another model.".into()
        )
    );
}

#[tokio::test]
async fn a_keyed_model_resolves_to_its_target_price_and_capabilities() {
    let t = ctx();
    let m = add(&t, "openai", "gpt-4o", None);
    assert_eq!(
        resolve(&t, Some(m.id)).await.unwrap_err(),
        ModelError::MissingCredential(
            "OpenAI requires an API key for GPT-4O. Add the provider's key in agent settings."
                .into()
        )
    );
    {
        let conn = t.ctx.sqlite.conn().unwrap();
        crate::agent::models::secrets::set(
            &conn,
            &t.ctx.security,
            "provider:openai",
            "sk-shared-key-1234",
        )
        .unwrap();
    }
    pass(&t, m.id);
    assert!(t.ctx.agent.resolver().is_configured(&t.ctx));
    let r = resolve(&t, None).await.unwrap();
    assert_eq!(
        (r.id, r.adapter, r.endpoint.as_str()),
        (m.id, AdapterKind::OpenAI, "https://api.openai.com/v1/")
    );
    assert_eq!(
        (r.model_name.as_str(), r.catalog_id.as_str()),
        ("gpt-4o", "openai/gpt-4o")
    );
    assert_eq!(r.context_window, Some(128_000));
    assert!(
        r.capabilities.vision,
        "the catalogue knows gpt-4o reads images"
    );
    assert!(!r.capabilities.reasoning);
    assert!(matches!(r.pricing, Pricing::Metered(Some(p)) if p.input == Some(0.000_002_5)));
    let auth = r.credentials.fetch().await.unwrap();
    assert_eq!(auth.key.unwrap().expose(), "sk-shared-key-1234");
    let conn = t.ctx.sqlite.conn().unwrap();
    let used: Option<String> = conn
        .query_row(
            "SELECT last_used_at FROM ag_secret WHERE name = 'provider:openai'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(used.is_some(), "a handed-over key is marked used");
    conn.execute("DELETE FROM ag_secret", []).unwrap();
    assert_eq!(
        r.credentials.fetch().await.unwrap_err(),
        ModelError::MissingCredential(
            "The API key for GPT-4O could not be read. Re-enter it in agent settings.".into()
        )
    );
}

#[tokio::test]
async fn a_row_left_at_off_takes_the_agents_default_effort() {
    let t = ctx();
    let m = add(&t, "ollama", "qwen3", Some("http://127.0.0.1:11434"));
    crate::agent::settings::update(
        &t.ctx,
        json!({"default_reasoning_effort": "high"})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    let r = resolve(&t, Some(m.id)).await.unwrap();
    assert_eq!(r.capabilities.default_reasoning_effort, "high");
    assert_eq!(
        (r.adapter, r.endpoint.as_str()),
        (AdapterKind::Ollama, "http://127.0.0.1:11434/")
    );
    assert!(
        r.credentials.fetch().await.unwrap().key.is_none(),
        "a local server takes no key"
    );
    let conn = t.ctx.sqlite.conn().unwrap();
    store::update(
        &conn,
        m.id,
        &store::ModelChanges {
            default_reasoning_effort: Some("low".into()),
            ..Default::default()
        },
        t.ctx.now(),
    )
    .unwrap();
    let r = resolve(&t, Some(m.id)).await.unwrap();
    assert_eq!(
        r.capabilities.default_reasoning_effort, "low",
        "the row's own level wins"
    );
}

#[tokio::test]
async fn a_provider_out_of_reach_is_refused_before_anything_is_sent() {
    let t = ctx();
    let m = add(&t, "litellm", "bedrock/anthropic.claude-3", None);
    let conn = t.ctx.sqlite.conn().unwrap();
    crate::agent::models::secrets::set(
        &conn,
        &t.ctx.security,
        "provider:litellm",
        "sk-shared-key-1234",
    )
    .unwrap();
    let e = resolve(&t, Some(m.id)).await.unwrap_err();
    assert!(
        matches!(e, ModelError::InvalidConfig(ref s) if s.contains("'bedrock'")),
        "{:?}",
        e
    );
}

struct SignedIn;

impl PlanSignIn for SignedIn {
    fn is_authorised(&self, _app: &AppState) -> bool {
        true
    }
    fn ensure_ready(&self, _app: &AppState) -> Result<(), String> {
        Ok(())
    }
    fn credentials(&self, _app: &Arc<AppState>) -> Arc<dyn ProviderCredentials> {
        Arc::new(NoCredentials)
    }
}

#[tokio::test]
async fn a_plan_model_runs_on_the_sign_in_and_is_never_priced() {
    let t = ctx();
    let m = add(&t, "litellm", "chatgpt/gpt-5.4", None);
    assert_eq!(
        resolve(&t, Some(m.id)).await.unwrap_err(),
        ModelError::MissingCredential(NOT_SIGNED_IN.into()),
        "the web's sign-in sentence until the plan part is installed"
    );
    assert_eq!(
        crate::agent::setup_gate::status(&t.ctx)["chatgpt_authorised"],
        json!(false)
    );
    t.ctx.agent.set_plan(Arc::new(SignedIn));
    let r = resolve(&t, Some(m.id)).await.unwrap();
    assert!(r.pricing.is_subscription());
    assert_eq!(r.adapter, AdapterKind::OpenAIResp);
    assert!(r.capabilities.reasoning, "the plan entry says it reasons");
    assert_eq!(
        crate::agent::setup_gate::status(&t.ctx)["chatgpt_authorised"],
        json!(true)
    );
}

#[tokio::test]
async fn the_setup_gate_answers_as_the_web() {
    let t = ctx();
    let s = crate::agent::setup_gate::status(&t.ctx);
    assert_eq!(
        s,
        json!({"status": "success", "configured": false, "model_count": 0, "default_model_id": null,
               "trading_enabled": false, "agent_available": true, "has_openalgo_api_key": false,
               "chatgpt_authorised": false})
    );
    let m = add(&t, "ollama", "llama3", Some("http://127.0.0.1:11434"));
    pass(&t, m.id);
    let s = crate::agent::setup_gate::status(&t.ctx);
    assert_eq!(
        (s["configured"].clone(), s["model_count"].clone()),
        (json!(true), json!(1))
    );
    assert_eq!(s["default_model_id"], json!(m.id));
}
