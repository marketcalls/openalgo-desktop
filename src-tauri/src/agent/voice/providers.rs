//! The speech providers the voice surface can mint a session with (the
//! web's `services/agent/voice_providers.py`).
//!
//! One entry today, OpenAI: its realtime model answers over WebRTC while
//! letting the app supply every answer itself (client delegation), which
//! keeps the trading intelligence on whatever model the trader configured.
//! The models and speakers listed are advisory: the model setting is free
//! text, so a newer model works the day it ships.

/// One speech provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceProviderSpec {
    /// The stored value of `voice_provider`.
    pub id: &'static str,
    /// What the trader sees.
    pub label: &'static str,
    /// Where a session is minted. A constant per provider, never taken from
    /// a request, which keeps the mint off the request-forgery surface.
    pub sessions_url: &'static str,
    /// The `ag_secret` row holding this provider's key; one per provider so
    /// a second provider never overwrites the first one's key.
    pub secret_name: &'static str,
    pub default_model: &'static str,
    /// Offered in the picker; advisory only.
    pub known_models: &'static [&'static str],
    /// Offered in the picker; advisory only.
    pub speakers: &'static [&'static str],
    /// The `delegation.type` that makes the provider answer nothing on its
    /// own and hand every question back to the app.
    pub delegation: &'static str,
}

/// Every provider, in declaration order.
pub const VOICE_PROVIDERS: &[VoiceProviderSpec] = &[VoiceProviderSpec {
    id: "openai",
    label: "OpenAI",
    sessions_url: "https://api.openai.com/v1/live/sessions",
    secret_name: "voice:openai",
    default_model: "gpt-live-1",
    known_models: &["gpt-live-1"],
    speakers: &["marin", "cedar", "alloy", "echo", "shimmer"],
    delegation: "client",
}];

/// The provider a fresh install uses.
pub const DEFAULT_VOICE_PROVIDER: &str = "openai";

/// One provider by id, in any casing; unknown ids are refused with the
/// web's sentence, naming what is accepted.
pub fn voice_provider_spec(provider: &str) -> Result<&'static VoiceProviderSpec, String> {
    let key = provider.trim().to_lowercase();
    VOICE_PROVIDERS.iter().find(|p| p.id == key).ok_or_else(|| {
        let accepted: Vec<&str> = VOICE_PROVIDERS.iter().map(|p| p.id).collect();
        format!(
            "Unknown voice provider {}. Accepted: {}",
            crate::agent::settings::pyrepr::repr_str(&key),
            accepted.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The web's vocabulary, pinned: the URL is a constant, the secret is
    /// per provider, delegation is client.
    #[test]
    fn the_one_provider_is_the_webs() {
        let spec = voice_provider_spec(" OpenAI ").unwrap();
        assert_eq!(spec.sessions_url, "https://api.openai.com/v1/live/sessions");
        assert_eq!(spec.secret_name, "voice:openai");
        assert_eq!(spec.default_model, "gpt-live-1");
        assert_eq!(spec.known_models, ["gpt-live-1"]);
        assert_eq!(
            spec.speakers,
            ["marin", "cedar", "alloy", "echo", "shimmer"]
        );
        assert_eq!(spec.delegation, "client");
        assert_eq!(DEFAULT_VOICE_PROVIDER, "openai");
        assert_eq!(
            voice_provider_spec("azure").unwrap_err(),
            "Unknown voice provider 'azure'. Accepted: openai"
        );
    }
}
