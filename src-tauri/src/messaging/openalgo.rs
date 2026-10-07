//! The account calls the bots make, run in-process.
//!
//! The web's bots call `/api/v1` through the OpenAlgo Python SDK with a
//! linked API key and a host URL. The bots here live inside the desktop, so
//! they call the same service functions the `/api/v1` handlers call, after
//! the same key check (`ApiKeyService`, plus a live broker session as the
//! web's `get_auth_token_broker` requires). No outbound request is ever made
//! with an API key, and a host URL given at linking is never contacted.
//! Each call answers what the endpoint would have answered.

use crate::security::Secret;
use crate::services::apikey_service::ApiKeyService;
use crate::services::core::{Reply, INVALID_API_KEY};
use crate::services::{account_service as account, market_data_service as market};
use crate::services::{order_service, Route};
use crate::state::AppState;
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct OpenAlgoClient {
    ctx: Arc<AppState>,
    api_key: Secret,
}

impl OpenAlgoClient {
    pub fn new(ctx: Arc<AppState>, api_key: Secret) -> Self {
        Self { ctx, api_key }
    }

    /// The web's `get_auth_token_broker`: a valid key and a live broker
    /// session, else the endpoint's 403 body.
    async fn authorized(&self) -> Result<(), Value> {
        let ctx = self.ctx.clone();
        let key = self.api_key.clone();
        // Argon2 off the async workers (the result is cached after the first).
        let valid =
            tokio::task::spawn_blocking(move || ApiKeyService::is_valid(&ctx, key.expose()))
                .await
                .unwrap_or(false);
        if valid && self.ctx.is_broker_connected() {
            Ok(())
        } else {
            Err(json!({"status": "error", "message": INVALID_API_KEY}))
        }
    }

    /// Whether the linked user's stored key is still valid and a broker is
    /// connected: the check every account action makes, for callers that
    /// change state without going through an endpoint (the mode button).
    pub async fn is_authorized(&self) -> bool {
        self.authorized().await.is_ok()
    }

    async fn run<F, Fut>(&self, f: F) -> Option<Value>
    where
        F: FnOnce(Arc<AppState>) -> Fut,
        Fut: std::future::Future<Output = Reply>,
    {
        if let Err(body) = self.authorized().await {
            return Some(body);
        }
        Some(f(self.ctx.clone()).await.body)
    }

    pub async fn funds(&self) -> Option<Value> {
        self.run(|c| async move { account::funds(&c).await }).await
    }
    pub async fn orderbook(&self) -> Option<Value> {
        self.run(|c| async move { account::orderbook(&c).await })
            .await
    }
    pub async fn tradebook(&self) -> Option<Value> {
        self.run(|c| async move { account::tradebook(&c).await })
            .await
    }
    pub async fn positionbook(&self) -> Option<Value> {
        self.run(|c| async move { account::positionbook(&c).await })
            .await
    }
    pub async fn holdings(&self) -> Option<Value> {
        self.run(|c| async move { account::holdings(&c).await })
            .await
    }
    pub async fn quotes(&self, symbol: &str, exchange: &str) -> Option<Value> {
        let (s, e) = (symbol.to_string(), exchange.to_string());
        self.run(|c| async move { market::quotes(&c, &s, &e).await })
            .await
    }
    /// SDK `closeposition()` (its default strategy name is "Python").
    pub async fn closeposition(&self) -> Option<Value> {
        self.run(|c| async move {
            order_service::close_position(&c, &json!({"strategy": "Python"}), Route::API).await
        })
        .await
    }
    pub async fn history(
        &self,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Option<Value> {
        let (s, e, i) = (
            symbol.to_string(),
            exchange.to_string(),
            interval.to_string(),
        );
        self.run(|c| async move { market::history(&c, &s, &e, &i, start, end, "api").await })
            .await
    }
}

/// `response.get("status") == "success"`.
pub fn is_success(v: &Option<Value>) -> bool {
    v.as_ref()
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
        == Some("success")
}
