//! The Agent (`/agent`, the `/trading` Assistant panel): an LLM that
//! answers from OpenAlgo's own services, at parity with OpenAlgo web's
//! `services/agent` (its build contract is the web's
//! `docs/design/55-agent/README.md`; where the two disagree the web code is
//! right).
//!
//! What lives where:
//!
//! - `contract/`: the interfaces every part codes against: the stream
//!   frames, the tool trait and its context, the resolved model and its
//!   resolver, the viz side channel, the approval hook.
//! - `tools/`: the registry, one line per toolkit, and the in-process
//!   `/api/v1` path the tools reach OpenAlgo through.
//! - `indicators/`: the 127 `openalgo.ta` indicators the indicator tools
//!   compute, on `oa_core`, with the web's registry and refusals.
//! - `prompts/`: the system prompt, its sections and the untrusted-text
//!   wrappers.
//! - `conversations/`: conversations, messages, the runs' own history and
//!   paused runs (migration `097_agent_core`).
//! - `attachments`: files on a turn, the vision check's input.
//! - `chart_catalogue`: the indicators the `/trading` chart can draw, from
//!   the generated `docs/prompt/indicators/chart-indicators.md`.
//! - `chart_contract`: the `/trading` chart wire contract both ways, the
//!   context the panel sends and the commands the chart tools send back;
//!   `chart_geometry`: levels, rails, zones and patterns from real bars.
//! - `tool_guard`: a call's arguments read, checked and coerced before the
//!   tool runs, and the correction the model reads when they do not fit.
//! - `redact`: secrets out of anything shown, stored or logged.
//! - `chat/`: the chat service the routes call; `run/`: one turn, its loop
//!   and its stored state; `stream/`: its frames as SSE; `reply`: a refusal
//!   before anything streams.
//!
//! Nothing a turn needs starts at boot: a run is built when the trader sends
//! a message, so a platform whose Agent cannot work still starts. [`start`]
//! closes runs a quit left open and sweeps expired approvals once a minute;
//! [`AgentModule::stop_all`] ends every turn at sign-out, lock, reset and
//! quit.

pub mod attachments;
pub mod chart_catalogue;
pub mod chart_contract;
pub mod chart_geometry;
pub mod chat;
pub mod contract;
pub mod conversations;
pub mod indicators;
pub mod prompts;
pub mod redact;
pub mod reply;
pub mod run;
pub mod stream;
#[cfg(test)]
pub(crate) mod testing;
pub mod tool_guard;
pub mod tools;
pub mod websearch;

use contract::approval::{ApprovalHook, NoApprovalRecord};
use contract::model::{ModelResolver, NoModelConfigured};
use contract::policy::{DefaultPolicy, RunPolicy, SectionSource};
use contract::tool::ToolGroup;
use parking_lot::{Mutex, RwLock};
use run::client::ModelClient;
use run::registry::RunRegistry;
use std::sync::Arc;
use std::time::Duration;
use stream::StreamSlots;

/// The Agent's state, owned by the app context.
pub struct AgentModule {
    resolver: RwLock<Arc<dyn ModelResolver>>,
    approvals: RwLock<Arc<dyn ApprovalHook>>,
    policy: RwLock<Arc<dyn RunPolicy>>,
    sections: RwLock<Vec<Arc<dyn SectionSource>>>,
    /// The toolkits runs are built from (the registry; a test's own).
    toolkits: RwLock<&'static [ToolGroup]>,
    /// The turns running now.
    runs: Arc<RunRegistry>,
    /// The answers streaming now.
    slots: StreamSlots,
    /// The provider connection, built on first use.
    client: Mutex<Option<Arc<ModelClient>>>,
    turn_limit: RwLock<Duration>,
}

impl Default for AgentModule {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentModule {
    pub fn new() -> Self {
        Self {
            resolver: RwLock::new(Arc::new(NoModelConfigured)),
            approvals: RwLock::new(Arc::new(NoApprovalRecord)),
            policy: RwLock::new(Arc::new(DefaultPolicy)),
            sections: RwLock::new(Vec::new()),
            toolkits: RwLock::new(tools::TOOLKITS),
            runs: Arc::new(RunRegistry::default()),
            slots: StreamSlots::default(),
            client: Mutex::new(None),
            turn_limit: RwLock::new(run::MAX_TURN),
        }
    }

    /// The toolkits a run is built from.
    pub fn toolkits(&self) -> &'static [ToolGroup] {
        *self.toolkits.read()
    }

    /// Build runs from other toolkits (a test's).
    #[cfg(test)]
    pub fn set_toolkits(&self, groups: &'static [ToolGroup]) {
        *self.toolkits.write() = groups;
    }

    /// The turns running now.
    pub fn runs(&self) -> Arc<RunRegistry> {
        self.runs.clone()
    }

    /// The stream slots.
    pub fn slots(&self) -> &StreamSlots {
        &self.slots
    }

    /// The longest a turn may run.
    pub fn turn_limit(&self) -> Duration {
        *self.turn_limit.read()
    }

    /// Cut turns off sooner (a test of the limit).
    #[cfg(test)]
    pub fn set_turn_limit(&self, limit: Duration) {
        *self.turn_limit.write() = limit;
    }

    /// The provider connection, built once.
    pub fn client(&self) -> Result<Arc<ModelClient>, String> {
        let mut slot = self.client.lock();
        if let Some(c) = slot.as_ref() {
            return Ok(c.clone());
        }
        let c = Arc::new(ModelClient::new()?);
        *slot = Some(c.clone());
        Ok(c)
    }

    /// End every turn now (sign-out, lock, account reset, fresh start,
    /// quit): each ends with `done` (cancelled) and is stored as cancelled.
    pub fn stop_all(&self) -> usize {
        let n = self.runs.cancel_all();
        if n > 0 {
            tracing::info!("Stopped {} Agent turn(s)", n);
        }
        n
    }

    /// Who decides a run's model (the config lane's, once installed).
    pub fn resolver(&self) -> Arc<dyn ModelResolver> {
        self.resolver.read().clone()
    }

    /// Install the model resolver (at startup, or a test's fake).
    pub fn set_resolver(&self, resolver: Arc<dyn ModelResolver>) {
        *self.resolver.write() = resolver;
    }

    /// Where approval decisions are recorded.
    pub fn approvals(&self) -> Arc<dyn ApprovalHook> {
        self.approvals.read().clone()
    }

    /// Install the approval hook (the orders lane's audit, a test's spy).
    pub fn set_approvals(&self, hook: Arc<dyn ApprovalHook>) {
        *self.approvals.write() = hook;
    }

    /// Where a run reads the trader's Agent settings.
    pub fn policy(&self) -> Arc<dyn RunPolicy> {
        self.policy.read().clone()
    }

    /// Install the settings reader (the config lane's).
    pub fn set_policy(&self, policy: Arc<dyn RunPolicy>) {
        *self.policy.write() = policy;
    }

    /// The registered prompt section sources, in registration order.
    pub fn section_sources(&self) -> Vec<Arc<dyn SectionSource>> {
        self.sections.read().clone()
    }

    /// Register a prompt section source (the skills lane's SKILLS and
    /// EXTERNAL DATA sections).
    pub fn add_section_source(&self, source: Arc<dyn SectionSource>) {
        self.sections.write().push(source);
    }
}

/// Once a minute, how often expired approvals are swept.
const SWEEP_EVERY: Duration = Duration::from_secs(60);

/// At startup: close runs a quit left `running` (they never finished, so
/// they are not history), then sweep expired approvals once a minute,
/// telling the approval hook each one will never resume. The task is owned
/// by the app and ends with it.
pub fn start(app: &Arc<crate::state::AppState>) {
    let me = Arc::downgrade(app);
    app.spawn(async move {
        if let Some(app) = me.upgrade() {
            let now = app.now();
            if let Err(e) = app
                .blocking(move |c| {
                    conversations::store::close_interrupted_runs(&*c.sqlite.conn()?, now)
                })
                .await
            {
                tracing::warn!(
                    "Agent runs left open by the last quit could not be closed: {}",
                    e
                );
            }
        }
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            let Some(app) = me.upgrade() else { return };
            if app.shutdown.is_cancelled() {
                return;
            }
            sweep(&app).await;
        }
    });
}

async fn sweep(app: &Arc<crate::state::AppState>) {
    let now = app.now();
    match app
        .blocking(move |c| conversations::store::expire_paused(&*c.sqlite.conn()?, now))
        .await
    {
        Ok(ids) => {
            let hook = app.agent.approvals();
            for id in ids {
                hook.abandoned(app, &id).await;
            }
        }
        Err(e) => tracing::warn!("Expired Agent approvals could not be swept: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::contract::approval::{Decision, DecisionScope};
    use super::contract::model::ModelError;
    use super::contract::policy::RunSettings;
    use super::contract::tool::{Capabilities, Surface};
    use super::prompts::PromptSection;
    use super::*;
    use crate::state::AppState;
    use async_trait::async_trait;

    struct Policy;
    impl RunPolicy for Policy {
        fn settings(&self, _: &AppState, _: Surface) -> RunSettings {
            RunSettings {
                trading_enabled: true,
                ..Default::default()
            }
        }
    }

    struct Skills;
    impl SectionSource for Skills {
        fn sections(&self, _: &AppState, _: Surface, _: &Capabilities) -> Vec<PromptSection> {
            vec![PromptSection::new("skills", "SKILLS", "x", 52)]
        }
    }

    struct Spy;
    #[async_trait]
    impl ApprovalHook for Spy {
        async fn decided(&self, _: &Arc<AppState>, _: &DecisionScope, _: &[Decision]) {}
        async fn abandoned(&self, _: &Arc<AppState>, _: &str) {}
    }

    /// Until a lane installs its own, a run is refused at the setup gate,
    /// trades nothing and adds no sections; installing replaces each.
    #[tokio::test]
    async fn each_hook_starts_safe_and_can_be_installed() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        let m = AgentModule::new();
        assert!(!m.resolver().is_configured(&t.ctx));
        assert!(matches!(
            m.resolver().resolve(&t.ctx, None).await,
            Err(ModelError::NotConfigured(_))
        ));
        assert!(!m.policy().settings(&t.ctx, Surface::Chat).trading_enabled);
        assert!(m.section_sources().is_empty());
        m.set_policy(Arc::new(Policy));
        m.add_section_source(Arc::new(Skills));
        m.set_approvals(Arc::new(Spy));
        m.set_resolver(Arc::new(NoModelConfigured));
        assert!(m.policy().settings(&t.ctx, Surface::Chat).trading_enabled);
        assert_eq!(m.section_sources().len(), 1);
    }
}
