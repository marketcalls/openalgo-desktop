//! Which model a run uses, and how its requests reach the provider (the
//! web's `builder.resolve_model` and `build_model`).
//!
//! The requested model, else the enabled default, else the setup gate's
//! refusal; a named model that is missing or switched off is refused, never
//! quietly swapped for the default. Everything is decided before the first
//! stream byte, so a failure is a clean answer. The key is not read here:
//! the run fetches it per request ([`StoredKey`]), so the plaintext lives
//! only for the request that sends it.

use crate::agent::catalog::{self, Catalog};
use crate::agent::contract::model::{
    ModelCapabilities, ModelError, ModelResolver, Prices, Pricing, ProviderAuth,
    ProviderCredentials, ResolvedModel, NOT_CONFIGURED_MESSAGE,
};
use crate::agent::models::secrets;
use crate::agent::models::store::{self, ModelRow};
use crate::agent::providers::target::target_for;
use crate::agent::providers::{
    capable, catalog_id, is_subscription, validate_provider_config, Probe,
};
use crate::agent::settings::schema::DEFAULT_REASONING_EFFORT;
use crate::agent::settings::Snapshot;
use crate::state::AppState;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

#[cfg(test)]
mod tests;

/// The config lane's [`ModelResolver`], installed at startup.
pub struct RegistryResolver;

fn internal(e: impl std::fmt::Display) -> ModelError {
    tracing::error!("The Agent's model could not be read: {}", e);
    ModelError::Internal("The configured models could not be read. Try again.".into())
}

/// The row a run asked for, or the default.
fn pick(app: &AppState, requested: Option<i64>) -> Result<ModelRow, ModelError> {
    let conn = app.sqlite.conn().map_err(internal)?;
    match requested {
        Some(id) => {
            let row = store::get(&conn, id).map_err(internal)?.ok_or_else(|| {
                ModelError::NotFound(format!(
                    "Model {} is not registered. Choose one of the configured models.",
                    id
                ))
            })?;
            if !row.enabled {
                return Err(ModelError::Disabled(format!(
                    "Model {} is disabled. Enable it in agent settings or choose another model.",
                    label(&row)
                )));
            }
            Ok(row)
        }
        None => store::default_model(&conn)
            .map_err(internal)?
            .ok_or_else(|| ModelError::NotConfigured(NOT_CONFIGURED_MESSAGE.into())),
    }
}

/// The name a refusal uses: the display name, else the model name, else
/// the id.
fn label(row: &ModelRow) -> String {
    [&row.display_name, &row.model_name]
        .iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| row.id.to_string())
}

/// Which stored key answers for a row (its own, else the provider's),
/// without opening it.
fn key_source(app: &AppState, row: &ModelRow) -> Result<Option<String>, ModelError> {
    let conn = app.sqlite.conn().map_err(internal)?;
    let found = secrets::describe(&conn, &row.secret_names()).map_err(internal)?;
    Ok(row
        .secret_names()
        .into_iter()
        .find(|n| found.get(n).is_some_and(|s| s.has_value)))
}

/// A row as the model a request uses: the gate, the plan's readiness, the
/// target, the capabilities the catalogue allows, the price. Used by a run
/// and by a credential test.
pub fn resolve_row(app: &Arc<AppState>, row: &ModelRow) -> Result<ResolvedModel, ModelError> {
    let shown = {
        let d = if row.display_name.trim().is_empty() {
            row.model_name.trim()
        } else {
            row.display_name.as_str()
        };
        if d.is_empty() {
            format!("model {}", row.id)
        } else {
            d.to_string()
        }
    };
    let source = key_source(app, row)?;
    if let Some(error) = validate_provider_config(
        &row.provider_kind,
        &row.model_name,
        row.base_url.as_deref(),
        source.is_some(),
    ) {
        if source.is_none() && error.contains("API key") {
            return Err(ModelError::MissingCredential(format!(
                "{} for {}. Add the provider's key in agent settings.",
                error, shown
            )));
        }
        return Err(ModelError::InvalidConfig(format!("{} ({}).", error, shown)));
    }
    let id = catalog_id(&row.provider_kind, &row.model_name).map_err(ModelError::InvalidConfig)?;
    let plan = is_subscription(&id);
    if plan {
        app.agent
            .plan()
            .ensure_ready(app)
            .map_err(ModelError::MissingCredential)?;
    }
    let catalog = catalog::current();
    let target = target_for(
        &catalog,
        &row.provider_kind,
        &row.model_name,
        row.base_url.as_deref(),
    )
    .map_err(ModelError::InvalidConfig)?;
    let credentials: Arc<dyn ProviderCredentials> = if plan {
        app.agent.plan().credentials(app)
    } else {
        Arc::new(StoredKey {
            app: Arc::downgrade(app),
            source,
            display_name: shown.clone(),
            marked: AtomicBool::new(false),
        })
    };
    if row.tools_unreliable {
        tracing::warn!(
            "Agent model {} is flagged tools_unreliable; this agent is tool driven and answers from it may be poor",
            shown
        );
    }
    let resolved = ResolvedModel {
        id: row.id,
        provider_kind: row.provider_kind.clone(),
        model_name: target.model,
        display_name: shown,
        catalog_id: id.clone(),
        adapter: target.adapter,
        endpoint: target.endpoint,
        credentials,
        capabilities: capabilities(app, &catalog, row, &id),
        context_window: catalog
            .model_meta(&id, None)
            .and_then(|m| m.entry)
            .and_then(|e| e.max_input_tokens)
            .and_then(|n| u32::try_from(n).ok()),
        pricing: if plan {
            Pricing::Subscription
        } else {
            pricing(&catalog, &id)
        },
    };
    tracing::info!(
        "Agent model resolved: {} ({}, reasoning={})",
        resolved.display_name,
        resolved.catalog_id,
        resolved.capabilities.reasoning
    );
    Ok(resolved)
}

/// What the model may be asked to do. A row left at `off` takes the
/// Agent's default reasoning effort (the web's `_reasoning_effort`: the
/// column's `off` is indistinguishable from "never chosen").
fn capabilities(app: &AppState, catalog: &Catalog, row: &ModelRow, id: &str) -> ModelCapabilities {
    const LEVELS: [&str; 3] = ["low", "medium", "high"];
    let own = row.default_reasoning_effort.trim().to_lowercase();
    let effort = if LEVELS.contains(&own.as_str()) {
        own
    } else {
        Snapshot::read(app)
            .text(DEFAULT_REASONING_EFFORT)
            .to_string()
    };
    ModelCapabilities {
        reasoning: capable(catalog, id, row.supports_reasoning, Probe::Reasoning),
        default_reasoning_effort: effort,
        vision: capable(catalog, id, row.supports_vision, Probe::Vision),
        tools_unreliable: row.tools_unreliable,
    }
}

/// The table's per-token prices for the model, or unknown (never a guess).
fn pricing(catalog: &Catalog, id: &str) -> Pricing {
    Pricing::Metered(
        catalog
            .model_meta(id, None)
            .and_then(|m| m.entry)
            .map(|e| Prices {
                input: e.input_cost,
                output: e.output_cost,
                cache_read: e.cache_read_cost,
            }),
    )
}

/// A stored key, opened for each request that sends it. The first use in
/// a run is recorded on the secret (the web marks a key used once per run).
struct StoredKey {
    app: Weak<AppState>,
    /// The secret that answered when the run was resolved; none for a
    /// keyless provider.
    source: Option<String>,
    display_name: String,
    marked: AtomicBool,
}

impl StoredKey {
    fn unreadable(&self) -> ModelError {
        ModelError::MissingCredential(format!(
            "The API key for {} could not be read. Re-enter it in agent settings.",
            self.display_name
        ))
    }
}

#[async_trait]
impl ProviderCredentials for StoredKey {
    async fn fetch(&self) -> Result<ProviderAuth, ModelError> {
        let Some(name) = self.source.clone() else {
            return Ok(ProviderAuth {
                key: None,
                headers: Vec::new(),
            });
        };
        let app = self.app.upgrade().ok_or_else(|| self.unreadable())?;
        let first = !self.marked.swap(true, Ordering::SeqCst);
        let opened = app
            .blocking(move |c| {
                let conn = c.sqlite.conn()?;
                let key = secrets::get(&conn, &c.security, &name);
                if key.is_some() && first {
                    secrets::mark_used(&conn, &name, c.now());
                }
                Ok(key)
            })
            .await
            .ok()
            .flatten();
        match opened {
            Some(key) => Ok(ProviderAuth {
                key: Some(key),
                headers: Vec::new(),
            }),
            None => Err(self.unreadable()),
        }
    }
}

#[async_trait]
impl ModelResolver for RegistryResolver {
    fn is_configured(&self, app: &AppState) -> bool {
        app.sqlite
            .conn()
            .and_then(|c| store::is_configured(&c))
            .unwrap_or_else(|e| {
                tracing::error!("Could not read the Agent configuration state: {}", e);
                false
            })
    }

    async fn resolve(
        &self,
        app: &Arc<AppState>,
        requested: Option<i64>,
    ) -> Result<ResolvedModel, ModelError> {
        let ctx = app.clone();
        app.blocking(move |c| Ok(pick(c, requested).and_then(|row| resolve_row(&ctx, &row))))
            .await
            .map_err(internal)?
    }
}

impl crate::agent::AgentModule {
    /// The Agent as the app runs it: the trader's registry decides a run's
    /// model and the trader's settings decide what it may do.
    pub fn configured() -> Self {
        let module = Self::new();
        module.set_resolver(Arc::new(RegistryResolver));
        module.set_policy(Arc::new(crate::agent::settings::policy::SettingsPolicy));
        module.add_section_source(Arc::new(crate::agent::skills::prompt::SkillsSection));
        module
    }
}
