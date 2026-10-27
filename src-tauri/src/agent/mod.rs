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
//! - `prompts/`: the system prompt, its sections and the untrusted-text
//!   wrappers.
//! - `conversations/`: conversations, messages, the runs' own history and
//!   paused runs (migration `097_agent_core`).
//! - `attachments`: files on a turn, the vision check's input.
//! - `tool_guard`: a call's arguments read, checked and coerced before the
//!   tool runs, and the correction the model reads when they do not fit.
//! - `redact`: secrets out of anything shown, stored or logged.
//!
//! Nothing starts at boot: a run is built when the trader sends a message,
//! so a platform whose Agent cannot work still starts.

pub mod attachments;
pub mod contract;
pub mod conversations;
pub mod prompts;
pub mod redact;
pub mod tool_guard;
pub mod tools;

use contract::approval::{ApprovalHook, NoApprovalRecord};
use contract::model::{ModelResolver, NoModelConfigured};
use contract::policy::{DefaultPolicy, RunPolicy, SectionSource};
use parking_lot::RwLock;
use std::sync::Arc;

/// The Agent's state, owned by the app context.
pub struct AgentModule {
    resolver: RwLock<Arc<dyn ModelResolver>>,
    approvals: RwLock<Arc<dyn ApprovalHook>>,
    policy: RwLock<Arc<dyn RunPolicy>>,
    sections: RwLock<Vec<Arc<dyn SectionSource>>>,
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
        }
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
