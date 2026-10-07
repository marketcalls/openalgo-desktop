//! Master contract status and the symbol cache (web
//! `blueprints/master_contract_status.py`), same paths and shapes.

use crate::brokers::types::AuthToken;
use crate::db::sqlite::master_contract_status as mcs;
use crate::server::envelope::json_response;
use crate::services::master_contract_service::{self, BUSY_MESSAGE};
use crate::state::{AppState, BrokerSession};
use axum::{extract::State, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn no_broker() -> Response {
    json_response(
        StatusCode::UNAUTHORIZED,
        json!({"status": "error", "message": "No broker session found"}),
    )
}

#[allow(clippy::result_large_err)]
fn status_value(ctx: &AppState, broker: &str) -> Result<Value, Response> {
    let row = ctx
        .sqlite
        .conn()
        .and_then(|c| mcs::get(&c, broker, ctx.now()))
        .map_err(|e| {
            tracing::error!("Reading the master contract status failed: {}", e);
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"status": "error", "message": "Failed to get master contract status"}),
            )
        })?;
    Ok(mcs::status_json(row.as_ref(), broker))
}

/// GET /api/master-contract/status
pub async fn status(State(ctx): Ctx) -> Response {
    let Some(s) = ctx.get_broker_session() else {
        return no_broker();
    };
    match status_value(&ctx, &s.broker_id) {
        Ok(v) => json_response(StatusCode::OK, v),
        Err(r) => r,
    }
}

/// GET /api/master-contract/ready
pub async fn ready(State(ctx): Ctx) -> Response {
    let Some(s) = ctx.get_broker_session() else {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({"ready": false, "message": "No broker session found"}),
        );
    };
    let ready = status_value(&ctx, &s.broker_id)
        .ok()
        .and_then(|v| v["is_ready"].as_bool())
        .unwrap_or(false);
    json_response(
        StatusCode::OK,
        json!({
            "ready": ready,
            "message": if ready { "Master contracts are ready" } else { "Master contracts not ready" },
        }),
    )
}

/// GET /api/master-contract/smart-status
pub async fn smart_status(State(ctx): Ctx) -> Response {
    let Some(s) = ctx.get_broker_session() else {
        return no_broker();
    };
    let mut v = match status_value(&ctx, &s.broker_id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let (should, reason) = master_contract_service::should_download(&ctx, &s.broker_id)
        .unwrap_or((true, "No previous download found".into()));
    let (h, m, tz) = master_contract_service::cutoff(&s.broker_id);
    v["smart_download"] = json!({
        "should_download": should,
        "reason": reason,
        "cutoff_time": format!("{:02}:{:02}", h, m),
        "cutoff_timezone": if tz == chrono_tz::UTC { "UTC" } else { "IST" },
    });
    json_response(StatusCode::OK, v)
}

fn auth_of(s: &BrokerSession) -> AuthToken {
    AuthToken::new(s.auth_token.expose())
        .with_feed(s.feed_token.as_ref().map(|t| t.expose().to_string()))
        .with_user_id(s.user_id.clone())
}

/// POST /api/master-contract/download (json `force`): the smart rule
/// unless forced; a download already running is refused with 409.
pub async fn download(State(ctx): Ctx, body: Option<axum::Json<Value>>) -> Response {
    let Some(s) = ctx.get_broker_session() else {
        return no_broker();
    };
    let force = body
        .as_ref()
        .and_then(|b| b.get("force"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !force {
        if let Ok((false, reason)) = master_contract_service::should_download(&ctx, &s.broker_id) {
            return json_response(
                StatusCode::OK,
                json!({"status": "skipped", "message": reason, "should_download": false}),
            );
        }
    }
    let Some(broker) = ctx.brokers.get(&s.broker_id) else {
        return no_broker();
    };
    if ctx.runtime.claims.is_running(&s.broker_id) {
        return json_response(
            StatusCode::CONFLICT,
            json!({"status": "error", "message": BUSY_MESSAGE, "started": false}),
        );
    }
    if let Ok(c) = ctx.sqlite.conn() {
        let _ = mcs::init_pending(&c, &s.broker_id, ctx.now());
    }
    let auth = auth_of(&s);
    let task_ctx = Arc::downgrade(&ctx);
    ctx.runtime.spawn_task(async move {
        let Some(ctx) = task_ctx.upgrade() else {
            return;
        };
        if master_contract_service::download(&ctx, &broker, &auth)
            .await
            .is_ok()
        {
            // Feed clients waiting on symbols are applied now.
            ctx.bridge.resync();
        }
    });
    json_response(
        StatusCode::OK,
        json!({"status": "success", "message": "Master contract download started", "started": true}),
    )
}

/// GET /api/cache/health
pub async fn cache_health(State(ctx): Ctx) -> Response {
    json_response(StatusCode::OK, master_contract_service::cache_health(&ctx))
}

/// POST /api/cache/reload: reload the stored master into memory.
pub async fn cache_reload(State(ctx): Ctx) -> Response {
    let Some(s) = ctx.get_broker_session() else {
        return no_broker();
    };
    if ctx.runtime.claims.is_running(&s.broker_id) {
        return json_response(
            StatusCode::CONFLICT,
            json!({"status": "error", "message": BUSY_MESSAGE}),
        );
    }
    match master_contract_service::load_cached(&ctx, &s.broker_id).await {
        Ok(n) if n > 0 => {
            ctx.bridge.resync();
            json_response(
                StatusCode::OK,
                json!({
                    "status": "success",
                    "message": format!("Cache reloaded successfully for broker: {}", s.broker_id),
                }),
            )
        }
        Ok(_) | Err(_) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"status": "error", "message": "Failed to reload cache"}),
        ),
    }
}
