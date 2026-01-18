//! Webhook and REST API server module
//!
//! Provides:
//! - Dynamic strategy-based webhooks (/webhook/{webhook_id})
//! - OpenAlgo SDK compatible REST API (/api/v1/*)
//!
//! Supports webhooks from:
//! - TradingView
//! - GoCharting
//! - Chartink
//!
//! Usage:
//! 1. Enable webhook server in settings
//! 2. Run ngrok: `ngrok http <port>`
//! 3. Configure ngrok URL in settings
//! 4. Create strategies with webhook_id
//! 5. Use the webhook URL: `<ngrok_url>/webhook/<webhook_id>`

mod server;
pub mod handlers;
mod types;

pub use server::WebhookServer;
pub use types::{
    // REST API types
    ApiResponse,
    PlaceOrderRequest,
    PlaceSmartOrderRequest,
    ModifyOrderRequest,
    CancelOrderRequest,
    CancelAllOrdersRequest,
    ClosePositionRequest,
    ApiKeyRequest,
    QuoteRequest,
    OrderData,
    TradeData,
    PositionData,
    HoldingData,
    FundsData,
    QuoteData,
    // Webhook types
    WebhookPayload,
    ProcessedAlert,
    WebhookResult,
    // Legacy (for backward compatibility)
    WebhookResponse,
    Empty,
};
