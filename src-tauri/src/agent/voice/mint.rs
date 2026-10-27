//! Minting a speech session (the web's `voice.mint_session`, `probe` and
//! `_refusal_message`).
//!
//! The only outbound call the voice surface makes: one POST that swaps the
//! window's SDP offer for the provider's answer. No audio passes through
//! OpenAlgo; the web view holds the WebRTC connection with the provider.
//! The URL is the provider's constant and the model, voice and persona come
//! from stored settings, so no request can steer the session or name the
//! host. The key goes in the `Authorization` header only, never the body,
//! a log or an error.

use super::config::{mark_key_used, voice_key, VoiceSettings};
use super::instructions::build_instructions;
use super::providers::VoiceProviderSpec;
use crate::state::AppState;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One round trip against a provider that is usually quick: long enough
/// for a slow handshake, short enough that a stuck provider does not hold
/// the request for a minute.
pub const MINT_TIMEOUT: Duration = Duration::from_secs(20);

pub const COULD_NOT_CONNECT: &str =
    "The microphone could not connect. Reload the page and try again.";
pub const SWITCHED_OFF: &str = "Voice is switched off. Turn it on in agent settings.";
pub const NO_KEY: &str = "No OpenAI key is saved for voice. Add one in agent settings.";
pub const UNREACHABLE: &str = "Could not reach OpenAI. Check this machine is online.";
pub const UNUSABLE: &str = "OpenAI sent back something we could not use. Try again.";
pub const NOTHING_TO_TEST: &str = "No OpenAI key is saved for voice, so there is nothing to test.";

/// Said whenever the cause is an empty account.
const NO_CREDIT: &str = "Your OpenAI account has run out of credit, so voice cannot start. Add credit at platform.openai.com under billing. Everything else keeps working if it runs on a different provider.";

/// Why a session could not be minted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintError {
    /// A sentence the trader can act on (the route's 400). Never carries
    /// the key or the provider's raw body.
    Unavailable(String),
    /// Something on this side failed (the route's 500), logged.
    Internal,
}

fn unavailable(message: &str) -> MintError {
    MintError::Unavailable(message.to_string())
}

/// The offer as the provider's parser needs it, or `None` when it is not
/// SDP: left-trimmed, starting `v=`, every line ending CRLF including the
/// last (a stripped final terminator comes back as "failed to unmarshal
/// SDP: EOF").
pub fn normalise_offer(raw: &str) -> Option<String> {
    let offer = raw.trim_start();
    if !offer.starts_with("v=") {
        return None;
    }
    let lf = offer.replace("\r\n", "\n");
    Some(format!(
        "{}\r\n",
        lf.trim_end_matches('\n').replace('\n', "\r\n")
    ))
}

/// A provider refusal as something the trader can act on. The body is read
/// for a cause but never echoed.
pub fn refusal_message(status: u16, body: &str) -> &'static str {
    match status {
        401 | 403 => "OpenAI would not accept your key. Check it in agent settings.",
        404 => "Your OpenAI account cannot use that voice. Pick another in agent settings.",
        // Out of credit and rate limited share a status and not a fix.
        429 if body.contains("insufficient_quota") || body.to_lowercase().contains("credit") => {
            NO_CREDIT
        }
        429 => "OpenAI is asking us to slow down. Wait a minute and try again.",
        // An exhausted balance arrives here as a bare 500, so the billing
        // check leads: it is the cause the trader can confirm and fix.
        s if s >= 500 => "Voice could not start, and OpenAI did not say why. Check your OpenAI credit first, at platform.openai.com under billing: when the credit runs out it fails exactly like this. If there is credit, the trouble is at OpenAI's end and it will pass.",
        _ if body.contains("not supported in realtime") || body.contains("invalid_model") => {
            "That voice cannot speak. Choose gpt-live-1 in agent settings."
        }
        _ => "Could not start voice. Try again in a minute.",
    }
}

/// The request body of one session: client delegation, the stored model
/// and voice, delivery instructions, and the offer.
pub fn session_payload(offer: &str, v: &VoiceSettings, spec: &VoiceProviderSpec) -> Value {
    let name = Some(v.agent_name.trim())
        .filter(|n| !n.is_empty())
        .unwrap_or("Vega");
    let model = Some(v.model.as_str())
        .filter(|m| !m.is_empty())
        .unwrap_or(spec.default_model);
    let speaker = Some(v.speaker.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(spec.speakers[0]);
    json!({
        "session": {
            "model": model,
            "instructions": build_instructions(name, v.trading_effective()),
            "audio": {"output": {"voice": speaker}},
            // The mode in which the speech model answers nothing itself:
            // every question comes back as session.delegation.created.
            "delegation": {"type": spec.delegation},
        },
        "transport": {"type": "webrtc", "sdp": offer},
    })
}

/// What a mint needs from the database: the settings, the provider and
/// its key.
struct Ready {
    settings: VoiceSettings,
    spec: &'static VoiceProviderSpec,
    key: crate::security::Secret,
}

async fn ready(app: &Arc<AppState>, force_enabled: bool) -> Result<Ready, MintError> {
    let read = app
        .blocking(|c| {
            let settings = VoiceSettings::read(c);
            let spec = settings.spec();
            let key = spec.as_ref().ok().and_then(|s| voice_key(c, s));
            Ok((settings, spec, key))
        })
        .await;
    let Ok((settings, spec, key)) = read else {
        tracing::error!("The voice settings could not be read");
        return Err(MintError::Internal);
    };
    if !(settings.enabled || force_enabled) {
        return Err(unavailable(SWITCHED_OFF));
    }
    // An unknown provider has no key to read (the web's `voice_key` reads
    // it as none).
    match (spec, key) {
        (Ok(spec), Some(key)) => Ok(Ready {
            settings,
            spec,
            key,
        }),
        _ => Err(unavailable(NO_KEY)),
    }
}

/// POST the payload once, and once more on a 5xx only: a session that was
/// not created was not billed, while a 4xx fails the same way again.
async fn post(
    client: &reqwest_agent::Client,
    url: &str,
    key: &str,
    payload: &Value,
) -> Result<reqwest_agent::Response, reqwest_agent::Error> {
    let send = || {
        client
            .post(url)
            .bearer_auth(key)
            .json(payload)
            .timeout(MINT_TIMEOUT)
            .send()
    };
    let first = send().await?;
    if first.status().is_server_error() {
        tracing::warn!(
            "Voice provider returned {}; retrying once",
            first.status().as_u16()
        );
        return send().await;
    }
    Ok(first)
}

/// Swap the window's SDP offer for the provider's answer.
/// `force_enabled` is the key test's: a key can be proven before voice is
/// switched on.
pub async fn mint_session(
    app: &Arc<AppState>,
    offer: &str,
    force_enabled: bool,
) -> Result<String, MintError> {
    let offer = normalise_offer(offer).ok_or_else(|| unavailable(COULD_NOT_CONNECT))?;
    let ready = ready(app, force_enabled).await?;
    let payload = session_payload(&offer, &ready.settings, ready.spec);
    let voice = app.agent.voice();
    let client = voice.client().map_err(|e| {
        tracing::error!("{}", e);
        MintError::Internal
    })?;
    let url = voice.sessions_url(ready.spec);
    let response = post(&client, &url, ready.key.expose(), &payload)
        .await
        .map_err(|_| {
            // Nothing from the error: it is not needed to act, and the
            // trader's next step is the sentence.
            tracing::error!("Could not reach the voice provider");
            unavailable(UNREACHABLE)
        })?;
    drop(ready.key);
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if status >= 400 {
        tracing::error!("Voice provider refused the session: HTTP {}", status);
        return Err(unavailable(refusal_message(status, &body)));
    }
    let answer = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["transport"]["sdp"].as_str().map(str::to_string))
        .ok_or_else(|| {
            tracing::error!("Voice provider returned an unusable session");
            unavailable(UNUSABLE)
        })?;
    let spec = ready.spec;
    if app
        .blocking(move |c| {
            mark_key_used(c, spec);
            Ok(())
        })
        .await
        .is_err()
    {
        tracing::warn!("Could not record voice key usage");
    }
    Ok(answer)
}

/// The result of testing the stored key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceProbe {
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
    pub model: String,
}

/// Mint a throwaway session against a fixed placeholder offer and drop it,
/// proving the key and the model (the provider checks the key before the
/// offer and the model after it). Works with voice still switched off.
pub async fn probe(app: &Arc<AppState>) -> Result<VoiceProbe, MintError> {
    let read = app
        .blocking(|c| {
            let v = VoiceSettings::read(c);
            let has_key = v.spec().ok().and_then(|s| voice_key(c, s)).is_some();
            Ok((v.model, has_key))
        })
        .await;
    let Ok((model, has_key)) = read else {
        return Err(MintError::Internal);
    };
    let model = Some(model)
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "gpt-live-1".into());
    if !has_key {
        return Ok(VoiceProbe {
            ok: false,
            message: NOTHING_TO_TEST.into(),
            latency_ms: 0,
            model,
        });
    }
    let started = Instant::now();
    let outcome = mint_session(app, PROBE_OFFER, true).await;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match outcome {
        Ok(_) => Ok(VoiceProbe {
            ok: true,
            message: format!("{} answered. Your key works.", model),
            latency_ms,
            model,
        }),
        Err(MintError::Unavailable(message)) => Ok(VoiceProbe {
            ok: false,
            message,
            latency_ms,
            model,
        }),
        Err(MintError::Internal) => Err(MintError::Internal),
    }
}

/// A complete, parseable WebRTC offer used only to prove a credential
/// (the web's `_PROBE_OFFER`): the provider checks the model after the
/// offer, so a malformed one would report a model that cannot speak as
/// working. Nothing connects to the session; the ICE credentials and
/// fingerprints are inert placeholders and carry no host address.
pub const PROBE_OFFER: &str = concat!(
    "v=0\r\n",
    "o=- 0 0 IN IP4 0.0.0.0\r\n",
    "s=-\r\n",
    "t=0 0\r\n",
    "a=group:BUNDLE 0 1\r\n",
    "a=msid-semantic:WMS *\r\n",
    "m=audio 9 UDP/TLS/RTP/SAVPF 96 9 0 8\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "a=sendrecv\r\n",
    "a=extmap:1 urn:ietf:params:rtp-hdrext:sdes:mid\r\n",
    "a=extmap:2 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r\n",
    "a=mid:0\r\n",
    "a=msid:probe probe\r\n",
    "a=rtcp:9 IN IP4 0.0.0.0\r\n",
    "a=rtcp-mux\r\n",
    "a=ssrc:350726838 cname:probe\r\n",
    "a=rtpmap:96 opus/48000/2\r\n",
    "a=rtpmap:9 G722/8000\r\n",
    "a=rtpmap:0 PCMU/8000\r\n",
    "a=rtpmap:8 PCMA/8000\r\n",
    "a=end-of-candidates\r\n",
    "a=ice-ufrag:probe\r\n",
    "a=ice-pwd:probeprobeprobeprobeprobe\r\n",
    "a=fingerprint:sha-256 10:B5:AC:80:AB:03:AC:BE:31:AA:73:30:BA:AB:62:80:07:18:1C:A8:D2:0E:17:B0:84:16:DC:E0:C9:AA:D3:50\r\n",
    "a=fingerprint:sha-384 14:CF:CF:50:3E:D6:33:43:82:88:ED:20:74:04:F6:80:B4:F7:6F:37:36:53:97:89:AE:30:1E:6C:CE:25:E0:54:45:B0:89:5A:F4:FB:0B:CC:6B:FB:24:8C:F8:A2:30:23\r\n",
    "a=fingerprint:sha-512 C6:8C:33:83:7C:C0:F5:71:48:6C:03:5F:64:07:C3:6B:BB:60:31:C6:F4:7B:0B:F9:23:6D:93:85:45:4C:32:0D:C2:CC:CD:8D:9A:3B:4E:AE:F3:82:B5:F0:4D:73:19:F5:BE:09:8D:12:16:06:16:ED:D4:ED:40:62:06:F0:F0:23\r\n",
    "a=setup:actpass\r\n",
    "m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "a=mid:1\r\n",
    "a=sctp-port:5000\r\n",
    "a=max-message-size:65536\r\n",
    "a=end-of-candidates\r\n",
    "a=ice-ufrag:probe\r\n",
    "a=ice-pwd:probeprobeprobeprobeprobe\r\n",
    "a=fingerprint:sha-256 10:B5:AC:80:AB:03:AC:BE:31:AA:73:30:BA:AB:62:80:07:18:1C:A8:D2:0E:17:B0:84:16:DC:E0:C9:AA:D3:50\r\n",
    "a=fingerprint:sha-384 14:CF:CF:50:3E:D6:33:43:82:88:ED:20:74:04:F6:80:B4:F7:6F:37:36:53:97:89:AE:30:1E:6C:CE:25:E0:54:45:B0:89:5A:F4:FB:0B:CC:6B:FB:24:8C:F8:A2:30:23\r\n",
    "a=fingerprint:sha-512 C6:8C:33:83:7C:C0:F5:71:48:6C:03:5F:64:07:C3:6B:BB:60:31:C6:F4:7B:0B:F9:23:6D:93:85:45:4C:32:0D:C2:CC:CD:8D:9A:3B:4E:AE:F3:82:B5:F0:4D:73:19:F5:BE:09:8D:12:16:06:16:ED:D4:ED:40:62:06:F0:F0:23\r\n",
    "a=setup:actpass\r\n",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offer_must_be_sdp_and_leaves_crlf_terminated() {
        for bad in [
            "",
            "   ",
            "hello",
            r#"{"sdp": "v=0"}"#,
            "o=- 0 0 IN IP4 0.0.0.0",
            "42",
        ] {
            assert_eq!(normalise_offer(bad), None, "{}", bad);
        }
        let lf = "v=0\no=- 0 0 IN IP4 0.0.0.0\ns=-";
        let sent = normalise_offer(&format!("  {}", lf)).unwrap();
        assert_eq!(sent, "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=-\r\n");
        assert_eq!(normalise_offer(PROBE_OFFER).unwrap(), PROBE_OFFER);
    }

    /// The web's TestAnUpstreamFailureSaysSo and the mint refusals.
    #[test]
    fn a_refusal_names_what_the_trader_can_fix() {
        assert!(refusal_message(401, "").contains("would not accept your key"));
        assert!(refusal_message(403, "{}").contains("would not accept your key"));
        assert!(refusal_message(404, "").contains("cannot use that voice"));
        assert!(refusal_message(429, "").contains("slow down"));
        assert!(!refusal_message(429, "slow down")
            .to_lowercase()
            .contains("credit"));
        let empty = refusal_message(429, r#"{"error":{"code":"insufficient_quota"}}"#);
        assert!(empty.contains("run out of credit") && !empty.contains("slow down"));
        for status in [500, 502, 503, 504] {
            let m = refusal_message(status, "Internal Server Error").to_lowercase();
            assert!(m.contains("credit") && m.contains("billing") && !m.contains("refused"));
        }
        assert!(
            refusal_message(400, r#"{"error":{"code":"invalid_model"}}"#).contains("cannot speak")
        );
        assert_eq!(
            refusal_message(400, "{}"),
            "Could not start voice. Try again in a minute."
        );
    }
}
