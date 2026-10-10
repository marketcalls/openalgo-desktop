//! API key page (web `blueprints/apikey.py`). Desktop server settings
//! live in `routes::settings`.

use crate::server::envelope::json_response;
use crate::server::form::FormData;
use crate::server::middleware::User;
use crate::server::routes::webui::no_store;
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// GET /apikey (Accept: application/json). Returns the key to the signed-in
/// session only, as the web does: the frontend sends it in `/api/v1` bodies.
/// Never cached (security review SEC-06).
pub async fn get_apikey(State(ctx): Ctx, User(u): User) -> Response {
    let key = match ApiKeyService::current(&ctx) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let mode = ApiKeyService::order_mode(&ctx).unwrap_or_else(|_| "auto".into());
    no_store(json_response(
        StatusCode::OK,
        json!({
            "login_username": u.username,
            "has_api_key": key.is_some(),
            "api_key": key.as_ref().map(|k| k.expose()),
            "order_mode": mode,
        }),
    ))
}

/// POST /apikey (json: user_id) -> new key.
pub async fn regenerate(State(ctx): Ctx, User(u): User, form: FormData) -> Response {
    if form.non_empty("user_id").is_none() {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({"error": "User ID is required"}),
        );
    }
    let c2 = ctx.clone();
    let name = u.username.clone();
    match tokio::task::spawn_blocking(move || ApiKeyService::regenerate(&c2, &name)).await {
        Ok(Ok(key)) => {
            tracing::info!("API key regenerated");
            no_store(json_response(
                StatusCode::OK,
                json!({
                    "message": "API key updated successfully.",
                    "api_key": key.expose(),
                    "key_id": 1,
                }),
            ))
        }
        _ => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to update API key"}),
        ),
    }
}

/// POST /apikey/mode (json: user_id, mode)
pub async fn set_mode(State(ctx): Ctx, form: FormData) -> Response {
    if form.non_empty("user_id").is_none() {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({"error": "User ID is required"}),
        );
    }
    let mode = form.non_empty("mode").unwrap_or_default();
    if mode != "auto" && mode != "semi_auto" {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({"error": "Invalid mode. Must be \"auto\" or \"semi_auto\""}),
        );
    }
    match ApiKeyService::set_order_mode(&ctx, &mode) {
        Ok(true) => json_response(
            StatusCode::OK,
            json!({"message": format!("Order mode updated to {}", mode), "mode": mode}),
        ),
        _ => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to update order mode"}),
        ),
    }
}
