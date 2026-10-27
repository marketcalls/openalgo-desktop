//! The voice surface (`docs/design/56-voice-agent` of the web): the same
//! agent, spoken to. The speech model hears and speaks and decides
//! nothing; every answer it reads out is a turn of the ordinary chat run on
//! the `voice` surface, with that surface's tools and limits.
//!
//! No audio passes through OpenAlgo: the app's web view holds the WebRTC
//! connection with the provider. This side mints the session, keeps the
//! settings and the key, decides spoken approvals and records the spoken
//! lines.
//!
//! - `providers`: the speech providers (one: OpenAI).
//! - `config`: the eight voice settings and the write-only key.
//! - `instructions`: what the speech model is told about delivery.
//! - `mint`: the one outbound call, and the key test.
//! - `confirm`: the pure matcher that decides whether words are a yes.
//! - `approval`: the in-memory windows a spoken yes can approve, opened
//!   only for runs started on the voice surface (D-017).
//! - `transcript`: every spoken line to the audit trail.
//!
//! Trading by voice is decided by the run builder from stored settings
//! (`chat::build`: the request asks, the master switch and the voice one
//! must both be on), never by the page.

pub mod approval;
pub mod config;
pub mod confirm;
pub mod instructions;
pub mod mint;
pub mod providers;
pub mod transcript;

#[cfg(test)]
mod tests;

use approval::ApprovalWindows;
use parking_lot::{Mutex, RwLock};
use providers::VoiceProviderSpec;
use transcript::TranscriptAudit;

/// The voice surface's state, owned by the Agent module.
pub struct VoiceModule {
    /// Spoken approval windows of paused voice runs.
    pub windows: ApprovalWindows,
    /// The mint's HTTP client (the Agent's own kind), built on first use.
    client: Mutex<Option<reqwest_agent::Client>>,
    audit: RwLock<TranscriptAudit>,
    /// A test's fake provider in place of the provider's sessions URL.
    #[cfg(test)]
    sessions_url: RwLock<Option<String>>,
}

impl Default for VoiceModule {
    fn default() -> Self {
        VoiceModule {
            windows: ApprovalWindows::default(),
            client: Mutex::new(None),
            audit: RwLock::new(transcript::not_installed()),
            #[cfg(test)]
            sessions_url: RwLock::new(None),
        }
    }
}

impl VoiceModule {
    /// The HTTP client the mint uses: one per process, no redirects, never
    /// a link-local address (`run::client::http_client`).
    pub fn client(&self) -> Result<reqwest_agent::Client, String> {
        let mut slot = self.client.lock();
        if let Some(c) = slot.as_ref() {
            return Ok(c.clone());
        }
        let c = crate::agent::run::client::http_client()?;
        *slot = Some(c.clone());
        Ok(c)
    }

    /// Where a session is minted: the provider's constant.
    pub fn sessions_url(&self, spec: &VoiceProviderSpec) -> String {
        #[cfg(test)]
        if let Some(url) = self.sessions_url.read().clone() {
            return url;
        }
        spec.sessions_url.to_string()
    }

    /// Point the mint at a test's fake provider.
    #[cfg(test)]
    pub(crate) fn set_sessions_url(&self, url: &str) {
        *self.sessions_url.write() = Some(url.to_string());
    }

    /// Where spoken lines are recorded.
    pub fn transcript_audit(&self) -> TranscriptAudit {
        self.audit.read().clone()
    }

    /// Install the audit trail's writer (the orders lane's, a test's spy).
    pub fn set_transcript_audit(&self, audit: TranscriptAudit) {
        *self.audit.write() = audit;
    }
}
