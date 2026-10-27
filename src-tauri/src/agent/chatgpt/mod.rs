//! The ChatGPT Plus or Pro plan as a second billing path (the web's
//! `services/agent/chatgpt_oauth.py` and `chatgpt_models.py`, and what its
//! LiteLLM `chatgpt` provider does for them).
//!
//! A model registered as `chatgpt/<name>` runs on the trader's plan, signed
//! in once through OpenAI's device flow, and its turns report tokens and no
//! cost; the same model as `openai/<name>` spends API credits. The prefix
//! of the resolved row is the only thing that decides which (D-026).
//!
//! - `flow`: the device flow's and the renewal's requests.
//! - `signin`: the one sign-in at a time, its snapshot and its poll task.
//! - `record`: the credential in the vault (`oauth:chatgpt`), canonical
//!   JSON, no token file (decision 10).
//! - `refresh`: the per-request credential, renewed 60 s early (D-025).
//! - `codex`: the request a plan turn sends, as LiteLLM sends it.
//! - `models`: the plan models the catalogue lacks.
//! - `service`: what the `/agent/api/chatgpt/*` routes answer.
//!
//! [`ChatGptPlan`] is installed as the Agent's [`PlanSignIn`]: the setup
//! gate, the model resolver and the model test ask it, and none of those
//! questions touches the network or starts a sign-in.

pub mod codex;
pub mod flow;
pub mod models;
pub mod record;
pub mod refresh;
pub mod service;
pub mod signin;

#[cfg(test)]
mod tests;

use crate::agent::contract::model::ProviderCredentials;
use crate::agent::plan::{PlanSignIn, NOT_SIGNED_IN};
use crate::state::AppState;
use signin::{unix, SignIn};
use std::sync::Arc;

/// The plan's sign-in, as the Agent sees it.
pub struct ChatGptPlan {
    sign_in: Arc<SignIn>,
}

impl ChatGptPlan {
    pub fn new(flow: flow::Flow) -> ChatGptPlan {
        ChatGptPlan {
            sign_in: Arc::new(SignIn::new(flow)),
        }
    }
}

impl Default for ChatGptPlan {
    fn default() -> Self {
        ChatGptPlan::new(flow::Flow::default())
    }
}

/// The stored credential can authenticate a request now (a refresh token,
/// or an unexpired access token). A database read, no network.
fn usable(app: &AppState) -> bool {
    app.sqlite
        .conn()
        .ok()
        .and_then(|c| record::load(&c, &app.security))
        .is_some_and(|r| r.has_credential(unix(app)))
}

impl PlanSignIn for ChatGptPlan {
    fn is_authorised(&self, app: &AppState) -> bool {
        usable(app)
    }

    fn ensure_ready(&self, app: &AppState) -> Result<(), String> {
        if usable(app) {
            Ok(())
        } else {
            Err(NOT_SIGNED_IN.into())
        }
    }

    fn credentials(&self, app: &Arc<AppState>) -> Arc<dyn ProviderCredentials> {
        Arc::new(refresh::PlanCredentials {
            app: Arc::downgrade(app),
            sign_in: self.sign_in.clone(),
        })
    }

    fn stop(&self) {
        self.sign_in.stop();
    }
}

/// The installed plan's sign-in, when the installed plan is this one (a
/// test may install another).
pub fn sign_in_of(app: &AppState) -> Option<Arc<SignIn>> {
    let plan = app.agent.plan();
    let any: &dyn std::any::Any = &*plan;
    any.downcast_ref::<ChatGptPlan>().map(|p| p.sign_in.clone())
}
