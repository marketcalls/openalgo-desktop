//! The catalogue's startup refresh: LiteLLM's price and context table
//! fetched once from where LiteLLM fetches it, with LiteLLM's 5 s timeout
//! and integrity rule, in place of the bundled copy (the web gets the same
//! table when LiteLLM is imported). Any failure keeps the bundled copy and
//! is one log line: the catalogue is advisory and the Agent runs without it.

use std::time::Duration;

/// LiteLLM's `model_cost_map_url`.
pub const TABLE_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// LiteLLM's fetch timeout.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// The largest table read (the real one is about 3 MB); a larger body is
/// refused before it is held, not after.
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

/// Put a fetched table in place, if it passes. True when it did.
pub fn apply(body: &[u8]) -> bool {
    match super::load::with_fetched(body) {
        Ok(next) => {
            super::replace(next);
            tracing::info!("The model catalogue was refreshed from the published price table");
            true
        }
        Err(reason) => {
            tracing::warn!(
                "The published model price table was not used ({}); the bundled catalogue stays",
                reason
            );
            false
        }
    }
}

/// Fetch the table: one attempt, the timeout over the whole exchange, and
/// the body read no further than [`MAX_BYTES`].
pub async fn fetch(client: &reqwest_agent::Client, url: &str) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("the request failed: {}", e.without_url()))?;
    if !response.status().is_success() {
        return Err(format!(
            "the server answered {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BYTES as u64)
    {
        return Err("the table is larger than expected".into());
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("the download failed: {}", e.without_url()))?
    {
        if body.len() + chunk.len() > MAX_BYTES {
            return Err("the table is larger than expected".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// At app start, once (an owned task, aborted at quit): fetch the table on
/// the Agent's own HTTP client and put it in place if it passes. Started by
/// the app only, never by a test context, so no test reaches the network.
pub fn start(app: &std::sync::Arc<crate::state::AppState>) {
    app.spawn(async move {
        let client = match crate::agent::run::client::http_client() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("The model catalogue was not refreshed: {}", e);
                return;
            }
        };
        match fetch(&client, TABLE_URL).await {
            Ok(body) => {
                if let Err(e) = tokio::task::spawn_blocking(move || apply(&body)).await {
                    tracing::warn!("The model catalogue refresh stopped: {}", e);
                }
            }
            Err(reason) => tracing::info!(
                "The model catalogue was not refreshed ({}); the bundled one is used",
                reason
            ),
        }
    });
}
