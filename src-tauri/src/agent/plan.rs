//! The ChatGPT plan's sign-in, as the model registry sees it.
//!
//! A `chatgpt/` model runs on the trader's ChatGPT Plus or Pro sign-in,
//! not on a key (the web's `chatgpt_oauth.py`). The sign-in itself, its
//! device flow, its token custody and refresh, belongs to the ChatGPT part
//! (the chatgpt-voice lane), which installs a [`PlanSignIn`] with
//! `app.agent.set_plan`. Until it does, no plan is signed in: the setup
//! gate says so and a plan model is refused with the web's sentence.

use crate::agent::contract::model::{ModelError, ProviderCredentials};
use crate::state::AppState;
use std::sync::Arc;

/// The web's `ensure_ready` refusal when no plan is signed in.
pub const NOT_SIGNED_IN: &str = "This model runs on a ChatGPT subscription and no subscription is authorised. Sign in to ChatGPT from agent settings.";

/// The ChatGPT plan's sign-in.
pub trait PlanSignIn: std::any::Any + Send + Sync {
    /// Whether a plan turn can authenticate now: no network, never a
    /// device login (the setup gate's `chatgpt_authorised`).
    fn is_authorised(&self, app: &AppState) -> bool;

    /// Before a plan model runs or is tested (the web's `ensure_ready`):
    /// ready, or the sentence the trader reads.
    fn ensure_ready(&self, app: &AppState) -> Result<(), String>;

    /// The credential each plan request fetches: the access token, renewed
    /// when it is about to expire, and the account header.
    fn credentials(&self, app: &Arc<AppState>) -> Arc<dyn ProviderCredentials>;

    /// The app signs out, locks, resets or quits: a sign-in waiting for
    /// approval stops (no network, no wait).
    fn stop(&self) {}
}

/// No plan signed in: what the app has until the ChatGPT part installs its
/// own.
pub struct NoPlanSignIn;

impl PlanSignIn for NoPlanSignIn {
    fn is_authorised(&self, _app: &AppState) -> bool {
        false
    }

    fn ensure_ready(&self, _app: &AppState) -> Result<(), String> {
        Err(NOT_SIGNED_IN.into())
    }

    fn credentials(&self, _app: &Arc<AppState>) -> Arc<dyn ProviderCredentials> {
        Arc::new(NotSignedIn)
    }
}

/// A plan request with no sign-in: refused before anything is sent.
struct NotSignedIn;

#[async_trait::async_trait]
impl ProviderCredentials for NotSignedIn {
    async fn fetch(&self) -> Result<crate::agent::contract::model::ProviderAuth, ModelError> {
        Err(ModelError::MissingCredential(NOT_SIGNED_IN.into()))
    }
}
