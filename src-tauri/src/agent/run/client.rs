//! The Agent's connection to model providers: its own HTTP client and the
//! genai client over it.
//!
//! The client is the Agent's alone, never the broker one: a turn may
//! stream for up to [`super::MAX_TURN`]. It is reqwest 0.13 with rustls on
//! ring and the webpki roots (no aws-lc, no OpenSSL), one per process,
//! handed to genai with `with_reqwest`. Each request names its provider's
//! endpoint, model and credentials itself ([`genai::ServiceTarget`]), so
//! one client serves every configured model and a key is fetched only for
//! the request that uses it.

use crate::agent::contract::model::{ModelError, ResolvedModel};
use genai::chat::{ChatOptions, ChatRequest, ChatStreamResponse, ReasoningEffort};
use genai::resolver::{AuthData, Endpoint};
use genai::{ModelIden, ServiceTarget};
use std::sync::Arc;
use std::time::Duration;

/// How long a provider may take to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a stream may stay silent before it is treated as broken. A
/// reasoning model can think for a few minutes before its first token.
const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// The Agent's HTTP client. It follows no redirect (a provider's API
/// answers where it is asked) and resolves names through the guarded
/// resolver, which never hands out a link-local address such as the cloud
/// metadata service's (`providers::egress`).
pub fn http_client() -> Result<reqwest_agent::Client, String> {
    http_client_with(crate::agent::providers::egress::GuardedResolver::system())
}

/// [`http_client`] on a given resolver (a test's lookup).
pub fn http_client_with<R: reqwest_agent::dns::Resolve + 'static>(
    resolver: R,
) -> Result<reqwest_agent::Client, String> {
    client_rules(resolver)?
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(super::MAX_TURN)
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| format!("The Agent's HTTP client could not be built: {}", e))
}

/// What every HTTP client of the Agent's is built on, before its own
/// timeouts: rustls on ring with the webpki roots, no redirect followed, and
/// names resolved through `resolver` (the external data client's too).
pub fn client_rules<R: reqwest_agent::dns::Resolve + 'static>(
    resolver: R,
) -> Result<reqwest_agent::ClientBuilder, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("TLS setup for the Agent failed: {}", e))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    // A preconfigured TLS config carries its own ALPN; without it reqwest
    // never negotiates HTTP/2.
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(reqwest_agent::Client::builder()
        .tls_backend_preconfigured(tls)
        .redirect(reqwest_agent::redirect::Policy::none())
        .dns_resolver(resolver))
}

/// One genai client for the process.
pub struct ModelClient {
    genai: genai::Client,
}

impl ModelClient {
    pub fn new() -> Result<ModelClient, String> {
        Ok(ModelClient {
            genai: genai::Client::builder()
                .with_reqwest(http_client()?)
                .build()
                .map_err(|e| format!("The Agent's model client could not be built: {}", e))?,
        })
    }

    /// Open one streamed model request.
    pub async fn stream(
        &self,
        model: &ResolvedModel,
        request: ChatRequest,
        effort: Option<&str>,
    ) -> Result<ChatStreamResponse, StreamOpenError> {
        self.stream_with(model, request, effort, |o| o).await
    }

    /// [`ModelClient::stream`] with the request's options adjusted last
    /// (a model Test caps the answer at one token:
    /// `|o| o.with_max_tokens(1)`).
    pub async fn stream_with(
        &self,
        model: &ResolvedModel,
        request: ChatRequest,
        effort: Option<&str>,
        adjust: impl FnOnce(ChatOptions) -> ChatOptions,
    ) -> Result<ChatStreamResponse, StreamOpenError> {
        let auth = model
            .credentials
            .fetch()
            .await
            .map_err(StreamOpenError::Credentials)?;
        // A keyless provider (a local server) still gets an empty bearer:
        // genai's OpenAI-format adapters require some value.
        let key = auth
            .key
            .as_ref()
            .map(|k| k.expose().to_string())
            .unwrap_or_default();
        let target = ServiceTarget {
            endpoint: Endpoint::from_owned(model.endpoint.clone()),
            auth: AuthData::from_single(key),
            model: ModelIden::new(model.adapter, model.model_name.clone()),
        };
        let mut options = ChatOptions::default()
            .with_capture_usage(true)
            .with_capture_content(true)
            .with_capture_reasoning_content(true)
            .with_capture_tool_calls(true)
            .with_stream_read_timeout(STREAM_READ_TIMEOUT);
        if let Some(e) = effort.and_then(reasoning_effort) {
            options = options.with_reasoning_effort(e);
        }
        if !auth.headers.is_empty() {
            options = options.with_extra_headers(auth.headers.clone());
        }
        let options = adjust(options);
        self.genai
            .exec_chat_stream(target, request, Some(&options))
            .await
            .map_err(StreamOpenError::Provider)
    }
}

/// Why a model request could not start.
#[derive(Debug)]
pub enum StreamOpenError {
    Credentials(ModelError),
    Provider(genai::Error),
}

fn reasoning_effort(level: &str) -> Option<ReasoningEffort> {
    match level {
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        _ => None,
    }
}

/// The reasoning effort a run sends (the web's `_reasoning_effort`): none
/// for a model that cannot reason; an explicit request wins (its `off`
/// included); else the model's own level (the config lane resolves a row
/// left at `off` to the Agent setting).
pub fn effective_effort(model: &ResolvedModel, requested: Option<&str>) -> Option<&'static str> {
    const LEVELS: [&str; 3] = ["low", "medium", "high"];
    if !model.capabilities.reasoning {
        return None;
    }
    if let Some(asked) = requested.map(|r| r.trim().to_lowercase()) {
        if let Some(l) = LEVELS.iter().find(|l| **l == asked) {
            return Some(l);
        }
        if asked == "off" {
            return None;
        }
    }
    let own = model
        .capabilities
        .default_reasoning_effort
        .trim()
        .to_lowercase();
    LEVELS.iter().find(|l| **l == own).copied()
}
