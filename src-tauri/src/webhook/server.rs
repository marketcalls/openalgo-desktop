//! HTTP server for webhooks and REST API
//!
//! Provides:
//! - Dynamic strategy-based webhooks (/webhook/{webhook_id})
//! - OpenAlgo SDK compatible REST API (/api/v1/*)

use crate::db::sqlite::WebhookConfig;
use crate::webhook::handlers::{self, WebhookState};
use axum::{
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{error, info};

/// Webhook/API server manager
pub struct WebhookServer {
    app_handle: AppHandle,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl WebhookServer {
    /// Create a new server
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            shutdown_tx: None,
        }
    }

    /// Start the server
    pub async fn start(&mut self, config: WebhookConfig) -> Result<(), String> {
        if !config.enabled {
            info!("Webhook/API server is disabled");
            return Ok(());
        }

        let host = config.host.clone();
        let port = config.port;

        // Parse address
        let addr: SocketAddr = format!("{}:{}", host, port)
            .parse()
            .map_err(|e| format!("Invalid address: {}", e))?;

        // Create shared state
        let state = Arc::new(WebhookState::new(self.app_handle.clone()));

        // Build CORS layer (allow all for local development)
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        // Build router with all routes
        let app = Router::new()
            // ================================================================
            // Health check
            // ================================================================
            .route("/health", get(handlers::health_check))
            .route("/", get(handlers::health_check))

            // ================================================================
            // Dynamic webhook endpoint (strategy-based)
            // POST /webhook/{webhook_id}
            // ================================================================
            .route("/webhook/:webhook_id", post(handlers::webhook_handler))

            // Legacy: Support /strategy/webhook/{webhook_id} for compatibility
            .route("/strategy/webhook/:webhook_id", post(handlers::webhook_handler))

            // ================================================================
            // REST API v1 (OpenAlgo SDK Compatible)
            // ================================================================

            // Order placement
            .route("/api/v1/placeorder", post(handlers::place_order))
            .route("/api/v1/placesmartorder", post(handlers::place_smart_order))
            .route("/api/v1/modifyorder", post(handlers::modify_order))
            .route("/api/v1/cancelorder", post(handlers::cancel_order))
            .route("/api/v1/cancelallorder", post(handlers::cancel_all_orders))
            .route("/api/v1/closeposition", post(handlers::close_position))

            // Data retrieval
            .route("/api/v1/orderbook", post(handlers::get_orderbook))
            .route("/api/v1/tradebook", post(handlers::get_tradebook))
            .route("/api/v1/positionbook", post(handlers::get_positionbook))
            .route("/api/v1/holdings", post(handlers::get_holdings))
            .route("/api/v1/funds", post(handlers::get_funds))
            .route("/api/v1/quotes", post(handlers::get_quotes))

            // ================================================================
            // Add state and middleware
            // ================================================================
            .with_state(state)
            .layer(cors)
            .layer(TraceLayer::new_for_http());

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        self.shutdown_tx = Some(shutdown_tx);

        // Start server
        info!("Starting OpenAlgo Desktop API server on {}", addr);

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", addr, e))?;

        // Spawn server task
        tokio::spawn(async move {
            let server = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                    info!("API server shutting down");
                });

            if let Err(e) = server.await {
                error!("API server error: {}", e);
            }
        });

        info!("OpenAlgo Desktop API server started successfully");
        info!("");
        info!("=== Endpoints ===");
        info!("");
        info!("Health Check:");
        info!("  GET  http://{}:{}/health", host, port);
        info!("");
        info!("Dynamic Webhook (strategy-based):");
        info!("  POST http://{}:{}/webhook/{{webhook_id}}", host, port);
        info!("");
        info!("REST API (OpenAlgo SDK compatible):");
        info!("  POST http://{}:{}/api/v1/placeorder", host, port);
        info!("  POST http://{}:{}/api/v1/placesmartorder", host, port);
        info!("  POST http://{}:{}/api/v1/modifyorder", host, port);
        info!("  POST http://{}:{}/api/v1/cancelorder", host, port);
        info!("  POST http://{}:{}/api/v1/cancelallorder", host, port);
        info!("  POST http://{}:{}/api/v1/closeposition", host, port);
        info!("  POST http://{}:{}/api/v1/orderbook", host, port);
        info!("  POST http://{}:{}/api/v1/tradebook", host, port);
        info!("  POST http://{}:{}/api/v1/positionbook", host, port);
        info!("  POST http://{}:{}/api/v1/holdings", host, port);
        info!("  POST http://{}:{}/api/v1/funds", host, port);
        info!("  POST http://{}:{}/api/v1/quotes", host, port);

        if let Some(ngrok_url) = config.ngrok_url {
            info!("");
            info!("=== Ngrok URL ===");
            info!("Base URL: {}", ngrok_url);
            info!("");
            info!("Webhook URL for strategies:");
            info!("  {}/webhook/{{your_webhook_id}}", ngrok_url);
            info!("");
            info!("REST API Base URL:");
            info!("  {}/api/v1/", ngrok_url);
        } else {
            info!("");
            info!("=== Ngrok Setup ===");
            info!("Run: ngrok http {}", port);
            info!("Then configure the ngrok URL in settings.");
        }

        Ok(())
    }

    /// Stop the server
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            info!("API server stop signal sent");
        }
    }

    /// Check if server is running
    pub fn is_running(&self) -> bool {
        self.shutdown_tx.is_some()
    }
}

impl Drop for WebhookServer {
    fn drop(&mut self) {
        self.stop();
    }
}
