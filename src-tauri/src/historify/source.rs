//! Where Historify's candles and notifications come from and go to.
//!
//! The job engine is written against these two traits so it can be driven by
//! the connected broker in the app and by a scripted broker in tests.

use crate::brokers::types::{Candle, HistoryRequest, QuoteKey};
use crate::events::{Event, EventBus};
use crate::state::AppState;
use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::Value;
use std::sync::{Arc, Weak};

/// Trader-facing text when no broker session is available.
pub const BROKER_NOT_CONNECTED: &str =
    "Your broker is not connected. Sign in to your broker, then download the data again.";

/// Broker history for one symbol and date range.
#[async_trait]
pub trait HistorySource: Send + Sync {
    /// `Err(trader message)` when downloads cannot run right now.
    fn ready(&self) -> Result<(), String>;

    /// Candles, or a trader-facing reason they could not be fetched.
    async fn fetch(
        &self,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Candle>, String>;
}

/// Socket.IO pushes (`historify_*`).
pub trait Notifier: Send + Sync {
    fn notify(&self, event: &'static str, payload: Value);
}

/// Published on the event bus; the Socket.IO subscriber emits it.
impl Notifier for EventBus {
    fn notify(&self, event: &'static str, payload: Value) {
        self.publish(Event::Historify { event, payload });
    }
}

/// The connected broker, as `/api/v1/history` reaches it.
pub struct BrokerSource {
    ctx: Weak<AppState>,
}

impl BrokerSource {
    pub fn new(ctx: Weak<AppState>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl HistorySource for BrokerSource {
    fn ready(&self) -> Result<(), String> {
        match self.ctx.upgrade() {
            Some(ctx) if ctx.get_broker_session().is_some() => Ok(()),
            _ => Err(BROKER_NOT_CONNECTED.to_string()),
        }
    }

    async fn fetch(
        &self,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Candle>, String> {
        let ctx: Arc<AppState> = self
            .ctx
            .upgrade()
            .ok_or_else(|| BROKER_NOT_CONNECTED.to_string())?;
        let h = crate::services::core::broker_handle(&ctx)
            .map_err(|_| BROKER_NOT_CONNECTED.to_string())?;
        crate::services::market_data_service::validate_symbol(&ctx, symbol, exchange)?;
        if end < start {
            return Ok(Vec::new());
        }
        let req = HistoryRequest {
            key: QuoteKey::new(exchange, symbol),
            interval: interval.to_string(),
            start,
            end,
        };
        h.broker.get_history(&h.auth, &req).await.map_err(|e| {
            tracing::warn!(
                "Historify download of {}:{} {} failed: {}",
                exchange,
                symbol,
                interval,
                e
            );
            e.client_message()
        })
    }
}
